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
        keys: "tab",
        description: "complete the command under the caret",
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
        keys: "/ · !",
        description: "run a command · run a shell line",
    },
    Binding {
        keys: "?",
        description: "toggle this panel",
    },
];

pub const FOOTER_HINT: &str = "? for shortcuts";
pub const FOOTER_MODES: &str = "/ commands · ! shell";
pub const PLACEHOLDER: &str = "ask anything · / for commands · ! for shell";

/// The panel, two columns wide when both fit with a gutter, else one.
pub fn help_lines(width: usize) -> Vec<Line<'static>> {
    let cells: Vec<String> = BINDINGS.iter().map(|b| cell(b, key_column())).collect();
    let cell_width = cells.iter().map(|c| c.width()).max().unwrap_or(0);
    let columns = if width >= cell_width * 2 + 6 { 2 } else { 1 };
    let rows = cells.len().div_ceil(columns);
    (0..rows)
        .map(|row| {
            let mut text = String::new();
            for column in 0..columns {
                let Some(cell) = cells.get(row + column * rows) else {
                    continue;
                };
                if column > 0 {
                    text.push_str(&" ".repeat(cell_width + 2 - text.width()));
                }
                text.push_str(cell);
            }
            Line::from(Span::styled(truncate(&text, width), theme::dim()))
        })
        .collect()
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
    fn no_row_overflows_the_width_it_was_built_for() {
        for width in [40usize, 80, 120, 200] {
            for line in help_lines(width) {
                assert!(line.to_string().width() <= width, "width {width}");
            }
        }
    }
}
