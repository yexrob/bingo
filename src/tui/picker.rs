//! The generic Picker core (data-driven; the interaction model distilled from `/think`, see notes/design/picker-model.md).
//!
//! Pure logic: no terminal / event loop / rendering — the row model (Line/Row) and theme are just data.
//! The scene shells (ThinkMenu/ModelMenu) own the differences: two-level structure, async loading, footer preview,
//! confirm actions — none of it merges into the core (the anti-over-abstraction line).
//!
//! Interaction contract (per slash-command-ux.md §3): `selected` is a pure browse index; effects only
//! land on the confirm key; Esc naturally has no rollback. Row rendering follows contract §3.3 (●/❯ dual markers + name/desc columns).

use crate::tui::chat::Row;
use crate::tui::line::{Line, SegStyle};
use crate::tui::theme::Theme;

/// A selectable item: label = display name, value = the committed value (written by Enter/s; may differ from the label,
/// e.g. /resume's label=display name, value=session key), description = a note column (nullable).
#[derive(Debug, Clone, PartialEq)]
pub struct PickerItem {
    pub label: String,
    pub value: String,
    pub description: String,
}

impl PickerItem {
    pub fn new(
        label: impl Into<String>,
        value: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
            description: description.into(),
        }
    }
}

/// Key/feedback configuration (optional capability toggles; all off by default, enabled explicitly by the scene shells).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PickerKeys {
    /// `s` key = this session only (does not write settings).
    pub session_only: bool,
    /// Number jump `1..=min(len, 9)`.
    pub number_jump: bool,
}

impl PickerKeys {
    /// Key-hint row text (assembled from the toggles; the number range = 1..=min(len, 9)).
    pub fn hint_text(&self, item_count: usize) -> String {
        let mut parts = vec![
            "↑↓ select".to_string(),
            if self.session_only {
                "Enter confirms & saves".to_string()
            } else {
                "Enter confirms".to_string()
            },
        ];
        if self.session_only {
            parts.push("s session only".to_string());
        }
        if self.number_jump {
            parts.push(format!("1-{} jumps", item_count.min(9)));
        }
        parts.push("Esc cancels".to_string());
        parts.join(" · ")
    }
}

/// Single-level selector state: items + a browse index (pure index — effects only land on the confirm key) + the active index
/// (● marker, stationary; None = the scene has no "currently active" concept).
#[derive(Debug, Clone, PartialEq)]
pub struct PickerModel {
    pub items: Vec<PickerItem>,
    pub selected: usize,
    pub current: Option<usize>,
}

impl PickerModel {
    /// Construct and clamp the indices: out-of-range selected/current fall back to 0 / None; empty items are guarded (the menu stays closed).
    pub fn new(items: Vec<PickerItem>, selected: usize, current: Option<usize>) -> Self {
        let current = current.filter(|i| *i < items.len());
        let selected = if items.is_empty() {
            0
        } else {
            selected.min(items.len() - 1)
        };
        Self {
            items,
            selected,
            current,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Browse step (wraps at both ends; no-op on an empty list, returns false).
    pub fn move_selection(&mut self, step: isize) -> bool {
        if self.items.is_empty() {
            return false;
        }
        let len = self.items.len();
        self.selected = (self.selected as isize + step).rem_euclid(len as isize) as usize;
        true
    }

    /// Number jump `1..=min(len, 9)`; out-of-range / empty list return false (the caller falls into the input-edit path).
    pub fn jump(&mut self, n: usize) -> bool {
        if self.items.is_empty() {
            return false;
        }
        if n == 0 || n > self.items.len().min(9) {
            return false;
        }
        self.selected = n - 1;
        true
    }

    /// The currently selected item (None on an empty list).
    pub fn selected_item(&self) -> Option<&PickerItem> {
        self.items.get(self.selected)
    }

    /// Row rendering (contract §3.3): 2-space indent + a ❯/● marker column + name column + desc column;
    /// narrow screens truncate the whole row to fit (fixed chrome budget = 2 indent + marker ≤4 + 2 separators).
    pub fn row(&self, index: usize, width: usize, theme: &Theme) -> Row {
        let Some(item) = self.items.get(index) else {
            return Row::new(Line::empty()); // empty / out-of-range guard: a blank row placeholder
        };
        let selected = index == self.selected;
        let current = self.current == Some(index);
        let mut line = Line::empty();
        // Marker column: ❯ = browsed selection (moves), ● = currently active (fixed); on overlap ❯ takes the prefix slot and ● sits before the name.
        line.push_styled("  ", SegStyle::fg(theme.text_secondary));
        if selected {
            line.push_styled("❯ ", SegStyle::fg(theme.permission));
            line.push_styled(
                if current { "● " } else { "  " },
                SegStyle::fg(theme.claude),
            );
        } else {
            line.push_styled("  ", SegStyle::fg(theme.text_secondary));
            line.push_styled(
                if current { "● " } else { "  " },
                SegStyle::fg(theme.claude),
            );
        }
        let name_style = if selected {
            theme.permission
        } else if current {
            theme.claude
        } else {
            theme.text_secondary
        };
        let name_col = self
            .items
            .iter()
            .map(|i| i.label.chars().count())
            .max()
            .unwrap_or(0);
        let name_text = format!("{:<width$}", item.label, width = name_col);
        // Fixed chrome: 2 indent + mark (≤4 on the overlap row) + 2 separator.
        let avail = width.saturating_sub(2 + 4 + 2);
        if avail >= name_text.chars().count() + 2 {
            // Normal width: name and desc each carry their own style.
            let desc_budget = avail - name_text.chars().count() - 2;
            line.push_styled(name_text, SegStyle::fg(name_style));
            line.push_styled("  ", SegStyle::fg(theme.text_secondary));
            line.push_styled(
                crate::tui::markdown::truncate(&item.description, desc_budget),
                SegStyle::fg(theme.text_secondary),
            );
        } else {
            // Narrow terminal: the whole name+desc segment truncates as one.
            line.push_styled(
                crate::tui::markdown::truncate(
                    &format!("{name_text}  {}", item.description),
                    avail,
                ),
                SegStyle::fg(name_style),
            );
        }
        Row::new(line)
    }

    /// Windowed rendering: past `max_rows` items, take a window around `selected`; the cut-off side
    /// gets a "… N more" indicator row — items past the window in long lists (e.g. dozens of models)
    /// stay reachable and visible (the old take(N) cut made out-of-window items unselectable forever).
    pub fn window_rows(&self, max_rows: usize, width: usize, theme: &Theme) -> Vec<Row> {
        let len = self.items.len();
        let max_rows = max_rows.max(1);
        if len <= max_rows {
            return (0..len).map(|i| self.row(i, width, theme)).collect();
        }
        let more_row = |n: usize| {
            Row::new(Line::styled(
                crate::tui::markdown::truncate(&format!("  … {n} more"), width.saturating_sub(2)),
                SegStyle::fg(theme.text_secondary),
            ))
        };
        let body = max_rows.saturating_sub(2).max(1);
        let start = self
            .selected
            .saturating_sub(body / 2)
            .min(len.saturating_sub(body));
        let end = start + body;
        let mut rows = Vec::with_capacity(max_rows);
        if start > 0 {
            rows.push(more_row(start));
        }
        rows.extend((start..end).map(|i| self.row(i, width, theme)));
        if end < len {
            rows.push(more_row(len - end));
        }
        rows
    }

    /// Key-hint row (dim; truncated on narrow screens; keys supplied by the scene shell).
    pub fn hint_row(&self, keys: PickerKeys, width: usize, theme: &Theme) -> Row {
        let hint = crate::tui::markdown::truncate(
            &keys.hint_text(self.items.len()),
            width.saturating_sub(2),
        );
        Row::new(Line::styled(
            format!("  {hint}"),
            SegStyle::fg(theme.text_secondary),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::line::text_width;

    fn item(label: &str) -> PickerItem {
        PickerItem::new(label, label, format!("desc of {label}"))
    }

    fn model(items: Vec<PickerItem>, selected: usize, current: Option<usize>) -> PickerModel {
        PickerModel::new(items, selected, current)
    }

    fn text(row: &Row) -> String {
        row.line.plain_text()
    }

    /// move_selection wraps at both ends; empty list is a no-op.
    #[test]
    fn move_selection_wraps_and_guards_empty() {
        let mut m = model(vec![item("a"), item("b"), item("c")], 0, None);
        assert!(m.move_selection(-1));
        assert_eq!(m.selected, 2, "wraps from the top to the last item");
        assert!(m.move_selection(1));
        assert_eq!(m.selected, 0, "wrapped around");
        assert!(m.move_selection(1));
        assert_eq!(m.selected, 1);
        let mut empty = model(Vec::new(), 0, None);
        assert!(!empty.move_selection(1), "empty list is a no-op");
        assert!(!empty.move_selection(-1));
    }

    /// jump: 1..=min(len,9) direct select; 0 / out-of-range / empty list return false without panicking (the caller falls into the input-edit path).
    #[test]
    fn jump_bounds_and_guards() {
        let mut m = model(vec![item("a"), item("b"), item("c")], 0, None);
        assert!(m.jump(3));
        assert_eq!(m.selected, 2, "3 jumps to item 3");
        assert!(m.jump(1));
        assert_eq!(m.selected, 0);
        assert!(!m.jump(0), "0 is invalid");
        assert!(!m.jump(4), "out-of-range is invalid");
        assert_eq!(m.selected, 0, "an invalid jump does not move the selection");
        let mut empty = model(Vec::new(), 0, None);
        assert!(!empty.jump(1), "an empty list is invalid");
    }

    /// With more than 9 items the number jump only covers the first 9 (number_jump's width limit).
    #[test]
    fn jump_caps_at_nine_items() {
        let items: Vec<PickerItem> = (0..12).map(|i| item(&format!("m{i}"))).collect();
        let mut m = model(items, 0, None);
        assert!(m.jump(9));
        assert_eq!(m.selected, 8);
        assert!(!m.jump(10), "items from 10 onward cannot be number-jumped");
        assert_eq!(m.selected, 8, "out-of-range does not move the selection");
    }

    /// Empty-items guards: is_empty, selected/current clamping, and rendering a blank row.
    #[test]
    fn empty_items_defense() {
        let m = model(Vec::new(), 5, Some(5));
        assert!(m.is_empty());
        assert_eq!(m.selected, 0, "out-of-range selected falls back");
        assert_eq!(m.current, None, "out-of-range current falls back");
        let theme = Theme::dark();
        assert!(
            text(&m.row(0, 80, &theme)).is_empty(),
            "an empty list renders a blank row"
        );
        assert!(m.selected_item().is_none());
    }

    /// Index clamping: out-of-range selected/current each fall back.
    #[test]
    fn indices_clamp_on_construction() {
        let m = model(vec![item("a"), item("b")], 9, Some(9));
        assert_eq!(m.selected, 1);
        assert_eq!(m.current, None);
        let m = model(vec![item("a"), item("b")], 0, Some(1));
        assert_eq!(m.current, Some(1));
    }

    /// value and label are separate: the committed value comes from value (e.g. /resume).
    #[test]
    fn selected_value_uses_value_field() {
        let items = vec![
            PickerItem::new("display name", "session key", "3 messages"),
            PickerItem::new("other", "other", ""),
        ];
        let mut m = model(items, 0, None);
        assert_eq!(
            m.selected_item().map(|i| i.label.as_str()),
            Some("display name")
        );
        assert_eq!(
            m.selected_item().map(|i| i.value.as_str()),
            Some("session key")
        );
        m.jump(2);
        assert_eq!(m.selected_item().map(|i| i.value.as_str()), Some("other"));
    }

    /// Row rendering: ● marks active, ❯ marks browsing (separate and overlapping), name column width, whole-row truncation on narrow screens.
    #[test]
    fn row_renders_dual_markers_and_truncates() {
        let theme = Theme::dark();
        let m = model(vec![item("off"), item("low"), item("medium")], 1, Some(0));
        let r0 = text(&m.row(0, 80, &theme));
        assert!(r0.contains("● off"), "● marks the active row: {r0}");
        assert!(
            !r0.contains('❯'),
            "the active row is not a browse row: {r0}"
        );
        let r1 = text(&m.row(1, 80, &theme));
        assert!(r1.starts_with("  ❯"), "❯ marks the browse row: {r1}");
        assert!(r1.contains("low"), "browse row name: {r1}");
        // Overlap: ❯ takes the prefix slot, ● sits before the name.
        let overlap = model(vec![item("a"), item("b")], 1, Some(1));
        let ro = text(&overlap.row(1, 80, &theme));
        assert!(
            ro.contains("❯ ● b"),
            "overlapping row carries both markers: {ro}"
        );
        // name column width = the longest label; every row stays ≤ width in total.
        for width in 10..40usize {
            for i in 0..m.items.len() {
                assert!(
                    text_width(&text(&m.row(i, width, &theme))) <= width,
                    "width={width}"
                );
            }
        }
    }

    /// window_rows: short lists show everything; long lists open a window around selected + a "N more" indicator.
    #[test]
    fn window_rows_follow_the_selection() {
        let theme = Theme::dark();
        let items: Vec<PickerItem> = (0..30).map(|i| item(&format!("m{i}"))).collect();
        let mut m = model(items, 0, Some(0));
        let short = model(vec![item("a"), item("b")], 0, None);
        assert_eq!(
            short.window_rows(10, 80, &theme).len(),
            2,
            "short lists show everything"
        );

        let rows = m.window_rows(10, 80, &theme);
        assert_eq!(rows.len(), 9, "window of 8 rows + a bottom indicator");
        assert!(text(&rows[0]).contains("m0"), "{}", text(&rows[0]));
        assert!(
            text(rows.last().unwrap()).contains("22 more"),
            "{}",
            text(rows.last().unwrap())
        );

        // Selection moved to the end: the window follows, an indicator appears on top, the selected row stays visible.
        m.selected = 29;
        let rows = m.window_rows(10, 80, &theme);
        assert!(text(&rows[0]).contains("more"), "{}", text(&rows[0]));
        assert!(
            rows.iter()
                .any(|r| text(r).contains('❯') && text(r).contains("m29")),
            "the selected row is inside the window"
        );
        // Middle: indicator rows on both sides.
        m.selected = 15;
        let rows = m.window_rows(10, 80, &theme);
        assert!(text(&rows[0]).contains("more"));
        assert!(text(rows.last().unwrap()).contains("more"));
    }

    /// hint_row: text assembled from the keys; truncated on narrow screens; the number_jump range = 1..=min(len,9).
    #[test]
    fn hint_row_builds_from_keys() {
        let theme = Theme::dark();
        let m = model(vec![item("a"), item("b"), item("c")], 0, None);
        let full = text(&m.hint_row(
            PickerKeys {
                session_only: true,
                number_jump: true,
            },
            80,
            &theme,
        ));
        assert!(
            full.contains(
                "↑↓ select · Enter confirms & saves · s session only · 1-3 jumps · Esc cancels"
            ),
            "hint assembly: {full}"
        );
        let minimal = text(&m.hint_row(PickerKeys::default(), 80, &theme));
        assert!(
            minimal.contains("↑↓ select · Enter confirms · Esc cancels"),
            "default keys: {minimal}"
        );
        assert!(!minimal.contains("s this session"), "no s key by default");
        assert!(!minimal.contains("jumps"), "no number jump by default");
        for width in 10..40usize {
            assert!(text_width(&text(&m.hint_row(PickerKeys::default(), width, &theme))) <= width);
        }
    }
}
