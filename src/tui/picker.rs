//! 通用 Picker 核心（数据驱动；交互模型从 `/think` 提炼，见 notes/design/picker-model.md）。
//!
//! 纯逻辑：不碰终端/事件循环/渲染层——行模型（Line/Row）与主题只是数据。
//! 场景壳层（ThinkMenu/ModelMenu）持有差异：两级结构、异步加载、footer 预览、
//! 确认动作——都不并入核心（反过度抽象底线）。
//!
//! 交互契约（沿 slash-command-ux.md §3）：`selected` 是纯浏览索引，效果只在
//! 确认键写入；Esc 天然无回滚。行渲染沿契约 §3.3 布局（●/❯ 双标记 + name/desc 列）。

use crate::tui::chat::Row;
use crate::tui::line::{Line, SegStyle};
use crate::tui::theme::Theme;

/// 一个可选项：label 显示名、value 落地值（Enter/s 写入，可与 label 不同，
/// 如 /resume 的 label=展示名、value=会话 key）、description 说明列（可空）。
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

/// 键位/反馈配置（可选能力开关；Picker 默认全关，场景壳层显式开启）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PickerKeys {
    /// `s` 键 = 仅本次会话（不写 settings）。
    pub session_only: bool,
    /// 数字直达 `1..=min(len, 9)`。
    pub number_jump: bool,
}

impl PickerKeys {
    /// 按键提示行文案（按开关拼装；数字范围 = 1..=min(len, 9)）。
    pub fn hint_text(&self, item_count: usize) -> String {
        let mut parts = vec![
            "↑↓ 选择".to_string(),
            if self.session_only {
                "Enter 确认并保存".to_string()
            } else {
                "Enter 确认".to_string()
            },
        ];
        if self.session_only {
            parts.push("s 仅本次会话".to_string());
        }
        if self.number_jump {
            parts.push(format!("1-{} 直达", item_count.min(9)));
        }
        parts.push("Esc 取消".to_string());
        parts.join(" · ")
    }
}

/// 单级选择器状态：items + 浏览索引（纯索引——效果只在确认键写入）+ 生效索引
/// （● 标记，固定不动；None = 场景无「当前生效」概念）。
#[derive(Debug, Clone, PartialEq)]
pub struct PickerModel {
    pub items: Vec<PickerItem>,
    pub selected: usize,
    pub current: Option<usize>,
}

impl PickerModel {
    /// 构造并夹取索引：selected/current 越界回落 0 / None；空 items 防御（菜单不开）。
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

    /// 浏览步进（两端 wrap；空列表无操作，返回 false）。
    pub fn move_selection(&mut self, step: isize) -> bool {
        if self.items.is_empty() {
            return false;
        }
        let len = self.items.len();
        self.selected = (self.selected as isize + step).rem_euclid(len as isize) as usize;
        true
    }

    /// 数字直达 `1..=min(len, 9)`；越界/空列表返回 false（调用方落输入编辑路径）。
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

    /// 当前选中项（空列表 None）。
    pub fn selected_item(&self) -> Option<&PickerItem> {
        self.items.get(self.selected)
    }

    /// 行渲染（契约 §3.3）：2 空格缩进 + ❯/● 标记列 + name 列 + desc 列；
    /// 窄屏整体截断保宽度（固定 chrome 预算 = 2 缩进 + 标记 ≤4 + 2 分隔）。
    pub fn row(&self, index: usize, width: usize, theme: &Theme) -> Row {
        let Some(item) = self.items.get(index) else {
            return Row::new(Line::empty()); // 空/越界防御：空行占位
        };
        let selected = index == self.selected;
        let current = self.current == Some(index);
        let mut line = Line::empty();
        // 标记列：❯ = 浏览选中（移动），● = 当前生效（固定）；重叠行 ❯ 占前缀位、● 在名前。
        line.push_styled("  ", SegStyle::fg(theme.inactive));
        if selected {
            line.push_styled("❯ ", SegStyle::fg(theme.permission));
            line.push_styled(
                if current { "● " } else { "  " },
                SegStyle::fg(theme.claude),
            );
        } else {
            line.push_styled("  ", SegStyle::fg(theme.inactive));
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
            theme.inactive
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
            // 常规宽度：name 与 desc 各自独立样式。
            let desc_budget = avail - name_text.chars().count() - 2;
            line.push_styled(name_text, SegStyle::fg(name_style));
            line.push_styled("  ", SegStyle::fg(theme.inactive));
            line.push_styled(
                crate::tui::markdown::truncate(&item.description, desc_budget),
                SegStyle::fg(theme.inactive),
            );
        } else {
            // 窄终端：整个 name+desc 段整体截断。
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

    /// 按键提示行（dim；窄屏截断；keys 由场景壳层提供）。
    pub fn hint_row(&self, keys: PickerKeys, width: usize, theme: &Theme) -> Row {
        let hint = crate::tui::markdown::truncate(
            &keys.hint_text(self.items.len()),
            width.saturating_sub(2),
        );
        Row::new(Line::styled(
            format!("  {hint}"),
            SegStyle::fg(theme.inactive),
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
        assert_eq!(m.selected, 2, "上端 wrap 到末项");
        assert!(m.move_selection(1));
        assert_eq!(m.selected, 0, "回绕");
        assert!(m.move_selection(1));
        assert_eq!(m.selected, 1);
        let mut empty = model(Vec::new(), 0, None);
        assert!(!empty.move_selection(1), "空列表无操作");
        assert!(!empty.move_selection(-1));
    }

    /// jump: 1..=min(len,9) 直达；0/越界/空列表返回 false 不 panic（落输入编辑路径）。
    #[test]
    fn jump_bounds_and_guards() {
        let mut m = model(vec![item("a"), item("b"), item("c")], 0, None);
        assert!(m.jump(3));
        assert_eq!(m.selected, 2, "3 直达第 3 项");
        assert!(m.jump(1));
        assert_eq!(m.selected, 0);
        assert!(!m.jump(0), "0 无效");
        assert!(!m.jump(4), "越界无效");
        assert_eq!(m.selected, 0, "无效跳转不改选中");
        let mut empty = model(Vec::new(), 0, None);
        assert!(!empty.jump(1), "空列表无效");
    }

    /// 9 项以上数字直达只覆盖前 9 个（number_jump 的位宽限制）。
    #[test]
    fn jump_caps_at_nine_items() {
        let items: Vec<PickerItem> = (0..12).map(|i| item(&format!("m{i}"))).collect();
        let mut m = model(items, 0, None);
        assert!(m.jump(9));
        assert_eq!(m.selected, 8);
        assert!(!m.jump(10), "第 10 项起不能数字直达");
        assert_eq!(m.selected, 8, "越界不改选中");
    }

    /// 空 items 防御：is_empty、selected/current 夹取、渲染空行。
    #[test]
    fn empty_items_defense() {
        let m = model(Vec::new(), 5, Some(5));
        assert!(m.is_empty());
        assert_eq!(m.selected, 0, "越界 selected 回落");
        assert_eq!(m.current, None, "越界 current 回落");
        let theme = Theme::dark();
        assert!(text(&m.row(0, 80, &theme)).is_empty(), "空列表行渲染为空行");
        assert!(m.selected_item().is_none());
    }

    /// 索引夹取：selected/current 越界各自回落。
    #[test]
    fn indices_clamp_on_construction() {
        let m = model(vec![item("a"), item("b")], 9, Some(9));
        assert_eq!(m.selected, 1);
        assert_eq!(m.current, None);
        let m = model(vec![item("a"), item("b")], 0, Some(1));
        assert_eq!(m.current, Some(1));
    }

    /// value 与 label 分离：落地值取 value（/resume 等场景）。
    #[test]
    fn selected_value_uses_value_field() {
        let items = vec![
            PickerItem::new("展示名", "会话key", "3 条消息"),
            PickerItem::new("other", "other", ""),
        ];
        let mut m = model(items, 0, None);
        assert_eq!(m.selected_item().map(|i| i.label.as_str()), Some("展示名"));
        assert_eq!(m.selected_item().map(|i| i.value.as_str()), Some("会话key"));
        m.jump(2);
        assert_eq!(m.selected_item().map(|i| i.value.as_str()), Some("other"));
    }

    /// 行渲染：● 标生效、❯ 标浏览（分离与重叠）、name 列宽、窄屏整体截断。
    #[test]
    fn row_renders_dual_markers_and_truncates() {
        let theme = Theme::dark();
        let m = model(vec![item("off"), item("low"), item("medium")], 1, Some(0));
        let r0 = text(&m.row(0, 80, &theme));
        assert!(r0.contains("● off"), "● 标生效行: {r0}");
        assert!(!r0.contains('❯'), "生效行非浏览: {r0}");
        let r1 = text(&m.row(1, 80, &theme));
        assert!(r1.starts_with("  ❯"), "❯ 标浏览行: {r1}");
        assert!(r1.contains("low"), "浏览行名: {r1}");
        // 重叠：❯ 占前缀位、● 在名前。
        let overlap = model(vec![item("a"), item("b")], 1, Some(1));
        let ro = text(&overlap.row(1, 80, &theme));
        assert!(ro.contains("❯ ● b"), "重叠行双标记: {ro}");
        // name 列宽 = 最长 label；每行总宽 ≤ width。
        for width in 10..40usize {
            for i in 0..m.items.len() {
                assert!(
                    text_width(&text(&m.row(i, width, &theme))) <= width,
                    "width={width}"
                );
            }
        }
    }

    /// hint_row：按 keys 拼装文案；窄屏截断；number_jump 范围 = 1..=min(len,9)。
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
            full.contains("↑↓ 选择 · Enter 确认并保存 · s 仅本次会话 · 1-3 直达 · Esc 取消"),
            "hint 拼装: {full}"
        );
        let minimal = text(&m.hint_row(PickerKeys::default(), 80, &theme));
        assert!(
            minimal.contains("↑↓ 选择 · Enter 确认 · Esc 取消"),
            "默认键位: {minimal}"
        );
        assert!(!minimal.contains("s 仅本次"), "默认无 s 键");
        assert!(!minimal.contains("直达"), "默认无数字直达");
        for width in 10..40usize {
            assert!(text_width(&text(&m.hint_row(PickerKeys::default(), width, &theme))) <= width);
        }
    }
}
