//! A drawn frame with the styles a snapshot cannot show, and the assertion
//! `docs/design/tui.md` §9 asks for: colour lands only where §4 says it may.
//! A snapshot pins the words; this pins where the eye is sent.

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::Style;

use crate::clock::Now;
use crate::theme::{self, Colors, Theme};
use crate::tree::Tree;
use crate::ui::Ui;
use crate::view;

/// Draw the same frame in another look — `NO_COLOR`, or `BINGO_ASCII=1`.
pub fn in_look(theme: Theme, draw: impl FnOnce() -> String) -> String {
    theme::with(theme, draw)
}

pub fn no_colour() -> Theme {
    Theme {
        colors: Colors::Plain,
        glyphs: &theme::UNICODE,
    }
}

pub fn ascii() -> Theme {
    Theme {
        colors: Colors::Ansi,
        glyphs: &theme::ASCII,
    }
}

/// One drawn frame, kept cell by cell.
pub struct Painted(ratatui::buffer::Buffer);

/// Draw a tree and keep the buffer, so a test can ask what colour a row is.
pub fn painted(width: u16, height: u16, tree: &Tree, ui: &Ui, now: Now) -> Painted {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("a test terminal");
    terminal
        .draw(|frame| view::draw(tree, ui, frame, now))
        .expect("a drawn frame");
    Painted(terminal.backend().buffer().clone())
}

impl Painted {
    /// The row carrying `needle`, as runs of text that share one style.
    pub fn row(&self, needle: &str) -> Vec<(String, Style)> {
        let area = self.0.area();
        let row = (0..area.height)
            .find(|y| self.text(*y).contains(needle))
            .unwrap_or_else(|| panic!("no row carries {needle:?}:\n{}", self.screen()));
        let mut runs: Vec<(String, Style)> = Vec::new();
        for x in 0..area.width {
            let cell = &self.0[(x, row)];
            match runs.last_mut() {
                Some((text, style)) if *style == cell.style() => text.push_str(cell.symbol()),
                _ => runs.push((cell.symbol().to_string(), cell.style())),
            }
        }
        // The padding a row ends in is the terminal's, not the view's.
        if let Some((text, _)) = runs.last_mut() {
            *text = text.trim_end().to_string();
        }
        while runs.last().is_some_and(|(text, _)| text.is_empty()) {
            runs.pop();
        }
        runs
    }

    /// Every run of that row a colour was spent on.
    pub fn coloured(&self, needle: &str) -> Vec<String> {
        self.row(needle)
            .into_iter()
            .filter(|(text, style)| !text.trim().is_empty() && spends_colour(*style))
            .map(|(text, _)| text.trim().to_string())
            .collect()
    }

    fn text(&self, row: u16) -> String {
        (0..self.0.area().width)
            .map(|x| self.0[(x, row)].symbol())
            .collect()
    }

    fn screen(&self) -> String {
        (0..self.0.area().height)
            .map(|y| self.text(y))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Weight is free; hue is what the token table rations.
fn spends_colour(style: Style) -> bool {
    style != theme::as_drawn(Style::new())
        && style != theme::as_drawn(theme::dim())
        && style != theme::as_drawn(theme::bold())
}

/// The row carrying `needle` is exactly these runs, in this order.
pub fn assert_row_styled(painted: &Painted, needle: &str, expected: &[(&str, Style)]) {
    let drawn = painted.row(needle);
    let want: Vec<(String, Style)> = expected
        .iter()
        .map(|(text, style)| ((*text).to_string(), theme::as_drawn(*style)))
        .collect();
    assert_eq!(drawn, want, "the row carrying {needle:?}");
}
