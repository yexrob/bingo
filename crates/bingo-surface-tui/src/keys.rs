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
        description: "send the message",
    },
    Binding {
        keys: "shift+enter",
        description: "newline (ctrl+j · alt+enter · \\ enter)",
    },
    Binding {
        keys: "esc",
        description: "close dialog, panel or menu · interrupt",
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
        description: "expand the latest result · again to fold",
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

    #[test]
    fn no_row_overflows_the_width_it_was_built_for() {
        for width in [40usize, 80, 120, 200] {
            for line in help_lines(width) {
                assert!(line.to_string().width() <= width, "width {width}");
            }
        }
    }
}
