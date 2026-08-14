//! Keyboard bindings as a single source of truth: the `?` panel, the footer
//! byline and the bundled docs all read this table, so a binding can never be
//! documented in one place and implemented in another.

use crate::tui::line::text_width;

/// One user-visible key binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Binding {
    /// Key combination, lowercase and `+`-joined (CC spelling).
    pub keys: &'static str,
    /// What it does, one short phrase.
    pub description: &'static str,
}

/// Every binding the TUI implements, in the order the `?` panel lists them.
pub const BINDINGS: &[Binding] = &[
    Binding {
        keys: "enter",
        description: "send message",
    },
    Binding {
        keys: "\\ + enter",
        description: "insert newline (or ctrl+j)",
    },
    Binding {
        keys: "esc",
        description: "close dialog/menu/panel · interrupt · esc esc clears",
    },
    Binding {
        keys: "ctrl+c",
        description: "interrupt · clear · twice to exit",
    },
    Binding {
        keys: "up/down",
        description: "prompt history (edit queued messages while busy)",
    },
    Binding {
        keys: "ctrl+a/e",
        description: "start / end of line",
    },
    Binding {
        keys: "home/end",
        description: "start / end of line",
    },
    Binding {
        keys: "ctrl+d",
        description: "delete char after caret",
    },
    Binding {
        keys: "alt+b/f",
        description: "move one word",
    },
    Binding {
        keys: "ctrl+w/u/k",
        description: "delete word / to start / to end",
    },
    Binding {
        keys: "ctrl+y",
        description: "paste back last deletion",
    },
    Binding {
        keys: "ctrl+r",
        description: "search prompt history (tab adopt · esc cancel)",
    },
    Binding {
        keys: "tab",
        description: "complete slash / bash history",
    },
    Binding {
        keys: "pgup/pgdn",
        description: "scroll transcript (fullscreen)",
    },
    Binding {
        keys: "mouse",
        description: "wheel scroll · click folds (fullscreen)",
    },
    Binding {
        keys: "ctrl+s",
        description: "stash input (empty input restores it)",
    },
    Binding {
        keys: "ctrl+_",
        description: "undo edit",
    },
    Binding {
        keys: "ctrl+o",
        description: "transcript view: full output (ctrl+e · / · q)",
    },
    Binding {
        keys: "ctrl+t",
        description: "toggle task list",
    },
    Binding {
        keys: "ctrl+g",
        description: "open agents / channels workspace",
    },
    Binding {
        keys: "ctrl+b",
        description: "background the running command · manage background agents",
    },
    Binding {
        keys: "ctrl+l",
        description: "redraw screen",
    },
    Binding {
        keys: "shift+tab",
        description: "cycle permission mode",
    },
    Binding {
        keys: "alt+t",
        description: "toggle extended thinking",
    },
    Binding {
        keys: "! / /",
        description: "shell mode / commands (empty input; esc exits shell)",
    },
    Binding {
        keys: "1-9 · s",
        description: "picker: jump · session-only apply",
    },
    Binding {
        keys: "?",
        description: "toggle this panel",
    },
];

/// Footer hint shown while idle (CC `? for shortcuts`).
pub const FOOTER_IDLE_HINT: &str = "? for shortcuts";
/// Footer hint that applies in both idle and busy states. Still literally true
/// after D82 — ctrl+o is how the folded content is reached — it just reaches it
/// in the transcript view instead of by rewriting the screen, so the wording
/// under every collapsed row stays as it is.
pub const FOOTER_EXPAND_HINT: &str = "ctrl+o to expand";
/// Empty-input placeholder (CC PromptInput placeholder).
pub const INPUT_PLACEHOLDER: &str = "Try \"fix a bug\" · / for commands · ! for shell";

/// Column width of the key part in the `?` panel.
fn keys_column() -> usize {
    BINDINGS
        .iter()
        .map(|b| text_width(b.keys))
        .max()
        .unwrap_or(0)
}

/// Render the `?` panel body: `{keys}  {description}` pairs, laid out in two
/// columns when the terminal is wide enough, capped at `max_rows` (the caller
/// derives that cap from the terminal height so the panel can never push the
/// canvas past it). A dropped tail is reported as `… +N more`.
pub fn help_lines(width: usize, max_rows: usize) -> Vec<String> {
    if max_rows == 0 {
        return Vec::new();
    }
    let key_w = keys_column();
    let cell = |b: &Binding| format!("{:<key_w$}  {}", b.keys, b.description, key_w = key_w);
    let cell_w = BINDINGS
        .iter()
        .map(|b| text_width(&cell(b)))
        .max()
        .unwrap_or(0);
    // Two columns only when both fit with a gutter and a left margin.
    let columns = if width >= cell_w * 2 + 6 { 2 } else { 1 };
    let rows_needed = BINDINGS.len().div_ceil(columns);
    let rows = rows_needed.min(max_rows);
    // A truncated panel spends its last row on the `… +N more` notice.
    let (rows, truncated) = if rows_needed > rows {
        (rows.saturating_sub(1), true)
    } else {
        (rows, false)
    };
    let mut out = Vec::new();
    for r in 0..rows {
        let mut line = String::new();
        for c in 0..columns {
            let Some(binding) = BINDINGS.get(r + c * rows) else {
                continue;
            };
            if c > 0 {
                let pad = cell_w + 2 - text_width(&line);
                line.push_str(&" ".repeat(pad));
            }
            line.push_str(&cell(binding));
        }
        // Must truncate: NoWrap never clips, so an over-wide line would be
        // wrapped by the terminal itself and the canvas row count would
        // immediately diverge from what the chrome expects.
        out.push(crate::tui::markdown::truncate(line.trim_end(), width));
    }
    if truncated {
        let shown = out.len() * columns;
        out.push(format!("… +{} more", BINDINGS.len().saturating_sub(shown)));
    } else {
        // Cross-link: the two help surfaces reference each other (they used
        // to be disjoint islands).
        out.push("commands: /help".to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ctrl_g_help_opens_the_workspace_directly() {
        let binding = BINDINGS
            .iter()
            .find(|binding| binding.keys == "ctrl+g")
            .unwrap_or_else(|| panic!("ctrl+g binding missing"));
        assert_eq!(binding.description, "open agents / channels workspace");
        assert!(!binding.description.contains("pick"));
    }

    /// ctrl+b reads the situation: a shell command running in the foreground is
    /// what it backgrounds, and only when there is none does it open the manager
    /// (D84). The panel names both, in the order the key tries them.
    #[test]
    fn ctrl_b_help_names_both_of_its_meanings() {
        let binding = BINDINGS
            .iter()
            .find(|binding| binding.keys == "ctrl+b")
            .unwrap_or_else(|| panic!("ctrl+b binding missing"));
        assert_eq!(
            binding.description,
            "background the running command · manage background agents"
        );
    }

    #[test]
    fn help_panel_fits_the_row_budget() {
        for max_rows in 1..30 {
            let lines = help_lines(100, max_rows);
            // The /help cross-link row rides along only on untruncated panels.
            assert!(lines.len() <= max_rows + 1, "max_rows={max_rows}");
        }
        assert!(help_lines(100, 0).is_empty(), "no budget = no panel");
    }

    #[test]
    fn help_panel_lists_every_binding_when_it_fits() {
        let lines = help_lines(200, BINDINGS.len());
        let joined = lines.join("\n");
        for binding in BINDINGS {
            assert!(joined.contains(binding.keys), "missing {}", binding.keys);
            assert!(
                joined.contains(binding.description),
                "missing {}",
                binding.description
            );
        }
        assert!(!joined.contains("more"), "nothing dropped: {joined}");
    }

    #[test]
    fn narrow_terminals_fall_back_to_one_column() {
        let wide = help_lines(200, 40);
        let narrow = help_lines(50, 40);
        assert!(narrow.len() > wide.len(), "narrow stacks into more rows");
        assert_eq!(
            narrow.len(),
            BINDINGS.len() + 1,
            "all bindings + /help cross-link row"
        );
    }

    #[test]
    fn truncated_panel_reports_the_remainder() {
        let lines = help_lines(50, 5);
        assert_eq!(lines.len(), 5);
        let last = lines.last().expect("notice");
        assert!(last.starts_with("… +"), "{last}");
    }

    #[test]
    fn panel_lines_stay_within_width() {
        for width in [20usize, 30, 40, 60, 80, 100, 120, 200] {
            for line in help_lines(width, 40) {
                assert!(text_width(&line) <= width, "width={width} line={line:?}");
            }
        }
    }
}
