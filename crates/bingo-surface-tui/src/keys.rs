//! The one binding table. The `?` panel and the footer hint both read it, so a
//! key can never be documented in one place and bound in another.

use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::theme;

pub struct Binding {
    /// The chord, lowercase and `+`-joined; ` · ` separates alternatives.
    pub keys: &'static str,
    pub description: &'static str,
}

/// In the order the panel lists them.
pub const BINDINGS: &[Binding] = &[
    Binding {
        keys: "enter",
        description: "send the message · open the focused block",
    },
    Binding {
        keys: "shift+enter",
        description: "newline (ctrl+j · alt+enter · \\ enter)",
    },
    Binding {
        // The stack of [`ESCAPES`], in the order it is obeyed; a test holds
        // the two together.
        keys: "esc",
        description: "sheet → card → dropdown → interrupt",
    },
    Binding {
        keys: "esc esc",
        description: "rewind to an earlier turn",
    },
    Binding {
        keys: "ctrl+c",
        description: "interrupt · clear input · twice to exit",
    },
    Binding {
        keys: "ctrl+d",
        description: "exit on an empty input",
    },
    Binding {
        keys: "up/down",
        description: "prompt history at the first and last line",
    },
    Binding {
        keys: "ctrl+a/e",
        description: "start / end of the line (home · end)",
    },
    Binding {
        keys: "alt+b/f",
        description: "move one word",
    },
    Binding {
        keys: "ctrl+w/u/k",
        description: "delete word · to line start · to line end",
    },
    Binding {
        keys: "pgup/pgdn",
        description: "scroll the transcript",
    },
    Binding {
        keys: "ctrl+g",
        description: "switch to a sub-agent's view",
    },
    Binding {
        keys: "ctrl+t",
        description: "plugin state (tasks, rooms)",
    },
    Binding {
        keys: "tab",
        description: "complete the command under the caret",
    },
    Binding {
        keys: "shift+tab",
        description: "cycle permission mode",
    },
    Binding {
        keys: "1-9 · y/a/n",
        description: "answer the open dialog",
    },
    Binding {
        keys: "ctrl+e",
        description: "expand the dialog's preview",
    },
    Binding {
        keys: "ctrl+o",
        description: "expand the latest result · again for all of it",
    },
    Binding {
        keys: "/ · !",
        description: "run a command · run a shell line",
    },
    Binding {
        keys: "?",
        description: "toggle this panel",
    },
];

pub const FOOTER_HINT: &str = "? for shortcuts";
pub const PLACEHOLDER: &str = "ask anything · / for commands · ! for shell";

// ---- the two ordered stacks (design §7) ---------------------------------

/// What `esc` closes, innermost first. It is a table rather than a chain of
/// conditions so that the order is one thing, tested once and printed in the
/// help exactly as it is obeyed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Escape {
    /// A sheet over the whole frame: help, the panel, the resume picker.
    Sheet,
    /// The card that is asking: leaving it is its own cancel or denial.
    Card,
    /// The command dropdown.
    Dropdown,
    /// Nothing is open, so the turn is what `esc` stops.
    Interrupt,
}

/// The stack, in the order it is closed, with the word the help prints.
pub const ESCAPES: &[(Escape, &str)] = &[
    (Escape::Sheet, "sheet"),
    (Escape::Card, "card"),
    (Escape::Dropdown, "dropdown"),
    (Escape::Interrupt, "interrupt"),
];

/// What is open when `esc` is pressed.
#[derive(Clone, Copy, Debug, Default)]
pub struct Open {
    pub sheet: bool,
    pub card: bool,
    pub dropdown: bool,
    pub busy: bool,
}

impl Open {
    fn has(self, rung: Escape) -> bool {
        match rung {
            Escape::Sheet => self.sheet,
            Escape::Card => self.card,
            Escape::Dropdown => self.dropdown,
            Escape::Interrupt => self.busy,
        }
    }
}

/// The innermost thing `esc` closes, and nothing at all when a person has
/// nothing open and nothing running.
pub fn escape(open: Open) -> Option<Escape> {
    ESCAPES
        .iter()
        .map(|(rung, _)| *rung)
        .find(|rung| open.has(*rung))
}

/// What `ctrl+c` does, which is not what `esc` does: it is the key that gets
/// you out, and it says so before it takes you (§7).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interrupt {
    /// A turn is running: stop it, and leave the composer alone.
    Turn,
    /// Something is half-typed: clear it.
    Clear,
    /// Nothing to stop and nothing to clear: say how to leave.
    Arm,
    /// It was said a moment ago: leave.
    Exit,
}

/// What is true when `ctrl+c` is pressed.
#[derive(Clone, Copy, Debug, Default)]
pub struct Pressed {
    pub busy: bool,
    pub typing: bool,
    /// A first `ctrl+c` inside the window said how to leave.
    pub armed: bool,
}

pub fn interrupt(at: Pressed) -> Interrupt {
    match (at.busy, at.typing, at.armed) {
        (true, _, _) => Interrupt::Turn,
        (false, true, _) => Interrupt::Clear,
        (false, false, true) => Interrupt::Exit,
        (false, false, false) => Interrupt::Arm,
    }
}

/// From this width the table pairs its rows; below it one column reads better
/// than two cut-off ones.
pub const TWO_COLUMNS: usize = 100;
/// Cells between the two columns.
const GUTTER: usize = 2;

/// The panel: two columns from [`TWO_COLUMNS`], else one. A cell that does not
/// fit its column is elided — a row that spilled would move every row below it.
pub fn help_lines(width: usize) -> Vec<Line<'static>> {
    let cells: Vec<String> = BINDINGS.iter().map(|b| cell(b, key_column())).collect();
    let columns = if width >= TWO_COLUMNS { 2 } else { 1 };
    let cell_width = (width - GUTTER * (columns - 1)) / columns;
    let rows = cells.len().div_ceil(columns);
    (0..rows)
        .map(|row| {
            let text = (0..columns)
                .filter_map(|column| cells.get(row + column * rows))
                .map(|cell| pad(&truncate(cell, cell_width), cell_width))
                .collect::<Vec<_>>()
                .join(&" ".repeat(GUTTER));
            Line::from(Span::styled(text.trim_end().to_string(), theme::dim()))
        })
        .collect()
}

fn pad(text: &str, width: usize) -> String {
    format!("{text}{}", " ".repeat(width.saturating_sub(text.width())))
}

/// Rows never wrap: a panel row that spilled would move every row below it.
fn truncate(text: &str, width: usize) -> String {
    if text.width() <= width {
        return text.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if used + w > width.saturating_sub(1) {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    out
}

fn key_column() -> usize {
    BINDINGS.iter().map(|b| b.keys.width()).max().unwrap_or(0)
}

fn cell(binding: &Binding, key_column: usize) -> String {
    format!(
        "{:<key_column$}  {}",
        binding.keys,
        binding.description,
        key_column = key_column
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_chord_is_listed_once() {
        let mut seen: Vec<&str> = BINDINGS.iter().map(|b| b.keys).collect();
        seen.sort_unstable();
        let unique = {
            let mut u = seen.clone();
            u.dedup();
            u
        };
        assert_eq!(seen, unique, "one key, one row");
    }

    #[test]
    fn a_narrow_terminal_falls_back_to_one_column() {
        assert_eq!(help_lines(40).len(), BINDINGS.len());
    }

    #[test]
    fn a_wide_terminal_pairs_the_rows() {
        assert_eq!(help_lines(160).len(), BINDINGS.len().div_ceil(2));
    }

    #[test]
    fn a_hundred_columns_is_where_the_second_column_starts() {
        assert_eq!(help_lines(TWO_COLUMNS - 1).len(), BINDINGS.len());
        assert_eq!(
            help_lines(TWO_COLUMNS).len(),
            BINDINGS.len().div_ceil(2),
            "the sheet pairs its rows at 100 columns"
        );
    }

    /// The order `esc` is obeyed in is the order the help prints, because
    /// there is only one of it.
    #[test]
    fn the_help_prints_the_escape_stack() {
        let stack = ESCAPES
            .iter()
            .map(|(_, word)| *word)
            .collect::<Vec<_>>()
            .join(" → ");
        let esc = BINDINGS
            .iter()
            .find(|binding| binding.keys == "esc")
            .expect("a row for esc");
        assert_eq!(esc.description, stack);
        assert!(
            help_lines(120)
                .iter()
                .any(|line| line.to_string().contains(&stack)),
            "the sheet prints it"
        );
    }

    /// One test per rung: what is innermost is what closes.
    #[test]
    fn esc_closes_the_innermost_thing_that_is_open() {
        let all = Open {
            sheet: true,
            card: true,
            dropdown: true,
            busy: true,
        };
        assert_eq!(escape(all), Some(Escape::Sheet));
        assert_eq!(
            escape(Open {
                sheet: false,
                ..all
            }),
            Some(Escape::Card)
        );
        assert_eq!(
            escape(Open {
                sheet: false,
                card: false,
                ..all
            }),
            Some(Escape::Dropdown)
        );
        assert_eq!(
            escape(Open {
                busy: true,
                ..Open::default()
            }),
            Some(Escape::Interrupt)
        );
        assert_eq!(escape(Open::default()), None, "and nothing to close");
    }

    /// One test per row of what ctrl+c does.
    #[test]
    fn ctrl_c_stops_a_turn_clears_a_line_and_then_leaves() {
        let pressed = |busy, typing, armed| {
            interrupt(Pressed {
                busy,
                typing,
                armed,
            })
        };
        assert_eq!(pressed(true, true, true), Interrupt::Turn);
        assert_eq!(pressed(false, true, true), Interrupt::Clear);
        assert_eq!(pressed(false, false, true), Interrupt::Exit);
        assert_eq!(pressed(false, false, false), Interrupt::Arm);
    }

    #[test]
    fn no_row_overflows_the_width_it_was_built_for() {
        for width in [40usize, 80, 120, 200] {
            for line in help_lines(width) {
                assert!(line.to_string().width() <= width, "width {width}");
            }
        }
    }
}
