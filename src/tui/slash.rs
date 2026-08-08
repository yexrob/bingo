//! Slash command metadata and pure menu/help transformations.

/// Slash command metadata: name, argument hint, and user-facing description.
pub type SlashCommand = (&'static str, &'static str, &'static str);

pub const COMMANDS: &[SlashCommand] = &[
    ("help", "", "显示可用命令"),
    ("clear", "", "清空对话，开始新会话（别名 /reset /new）"),
    ("compact", "", "压缩上下文（旧消息 → 摘要）"),
    ("model", "[名称]", "显示/切换模型"),
    ("resume", "[名称或关键词]", "恢复历史会话"),
    ("rename", "[名称]", "重命名当前会话"),
    (
        "share",
        "[--public] [--open]",
        "导出 HTML；--public 才发布公网链接",
    ),
    ("context", "", "显示上下文用量"),
    ("status", "", "显示会话状态（模型/权限/会话/上下文）"),
    ("config", "", "显示生效配置与来源（层/环境变量/端点）"),
    (
        "permissions",
        "[allow|deny|ask] [规则]",
        "列出/添加权限规则",
    ),
    ("theme", "[dark|light|auto]", "切换主题"),
    ("mcp", "[enable|disable|reconnect]", "管理 MCP 服务器"),
    ("provider", "[名称]", "列出/切换 API provider"),
    ("think", "[off|low|medium|high|xhigh|max]", "设置思考级别"),
    ("skills", "", "列出可用技能"),
    ("tasks", "", "列出后台任务"),
    ("team", "start|status|assign|stop|list", "管理项目团队"),
    ("exit", "", "退出会话"),
];

/// Slash commands that execute immediately while a model turn is active.
pub const INSTANT_COMMANDS: &[&str] = &[
    "think", "model", "provider", "theme", "status", "context", "tasks", "help", "skills", "config",
];

/// Slash dropdown suggestion item (`/name`, hint, and description).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlashSuggestion {
    pub name: String,
    pub hint: String,
    pub description: String,
}

/// Result of rebuilding the slash dropdown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlashSuggestions {
    pub items: Vec<SlashSuggestion>,
    pub no_match: bool,
}

/// Formats `/help` directly from the command registry.
pub fn help_lines(commands: &[SlashCommand]) -> Vec<String> {
    let mut lines = vec!["可用命令：".to_string()];
    let command_width = commands
        .iter()
        .map(|(name, hint, _)| {
            name.chars().count() + usize::from(!hint.is_empty()) + hint.chars().count()
        })
        .max()
        .unwrap_or(0);
    for (name, hint, description) in commands {
        let command = if hint.is_empty() {
            format!("/{name}")
        } else {
            format!("/{name} {hint}")
        };
        lines.push(format!("  {command:<command_width$} — {description}"));
    }
    // Sub-commands invisible to the dropdown, and the cross-link to the key
    // panel (the two help surfaces used to be disjoint islands).
    lines.push(
        "  /provider login <名称> [--device-auth|--manual <token>] · /provider logout <名称>"
            .to_string(),
    );
    lines.push("快捷键：空输入按 ? 查看全表".to_string());
    lines
}

/// Builds slash suggestions from the input and an already-loaded set of extra entries
/// such as skills. Prefix matches rank before substring matches; ties prefer shorter names.
pub fn suggestions(
    input: &str,
    commands: &[SlashCommand],
    extras: impl IntoIterator<Item = SlashSuggestion>,
    max_items: usize,
) -> SlashSuggestions {
    let input = input.trim_end();
    let Some(query) = input.strip_prefix('/') else {
        return SlashSuggestions {
            items: Vec::new(),
            no_match: false,
        };
    };
    if query.contains(char::is_whitespace) {
        return SlashSuggestions {
            items: Vec::new(),
            no_match: false,
        };
    }

    let mut items: Vec<SlashSuggestion> = commands
        .iter()
        .map(|(name, hint, description)| SlashSuggestion {
            name: (*name).to_string(),
            hint: (*hint).to_string(),
            description: (*description).to_string(),
        })
        .chain(extras)
        .collect();
    let normalized = query.to_lowercase();
    if !normalized.is_empty() {
        items.retain(|suggestion| {
            let name = suggestion.name.to_lowercase();
            name.starts_with(&normalized) || name.contains(&normalized)
        });
        items.sort_by(|left, right| {
            let left_prefix = left.name.to_lowercase().starts_with(&normalized);
            let right_prefix = right.name.to_lowercase().starts_with(&normalized);
            right_prefix
                .cmp(&left_prefix)
                .then(left.name.len().cmp(&right.name.len()))
        });
    }
    items.truncate(max_items);
    SlashSuggestions {
        no_match: !normalized.is_empty() && items.is_empty(),
        items,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMMANDS: &[SlashCommand] = &[
        ("help", "", "显示可用命令"),
        ("model", "[名称]", "显示/切换模型"),
        ("status", "", "显示会话状态"),
    ];

    #[test]
    fn help_is_derived_from_registry() {
        let lines = help_lines(COMMANDS);
        assert_eq!(
            lines.len(),
            COMMANDS.len() + 3,
            "标题 + 命令 + 子命令行 + 快捷键互链"
        );
        assert_eq!(lines[0], "可用命令：");
        for ((name, hint, description), line) in COMMANDS.iter().zip(&lines[1..]) {
            assert!(line.contains(&format!("/{name}")), "命令存在: {line}");
            assert!(line.contains(hint), "参数提示存在: {line}");
            assert!(line.ends_with(description), "描述来自注册表: {line}");
        }
    }

    #[test]
    fn suggestions_filter_rank_merge_and_cap() {
        let extras = vec![SlashSuggestion {
            name: "model-check".to_string(),
            hint: String::new(),
            description: "技能".to_string(),
        }];
        let result = suggestions("/mo", COMMANDS, extras, 2);
        assert_eq!(
            result
                .items
                .iter()
                .map(|item| item.name.as_str())
                .collect::<Vec<_>>(),
            vec!["model", "model-check"]
        );
        assert!(!result.no_match);

        let empty = suggestions("/missing", COMMANDS, Vec::new(), 5);
        assert!(empty.items.is_empty());
        assert!(empty.no_match);
    }

    #[test]
    fn suggestions_only_open_for_argument_free_slash_input() {
        assert!(
            suggestions("hello", COMMANDS, Vec::new(), 5)
                .items
                .is_empty()
        );
        assert!(
            suggestions("/model name", COMMANDS, Vec::new(), 5)
                .items
                .is_empty()
        );
        assert_eq!(suggestions("/", COMMANDS, Vec::new(), 2).items.len(), 2);
    }
}
