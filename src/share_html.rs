//! share 页面的 HTML 渲染（`bingo share` 输出 · v3.0 opencode 完全复刻）。
//!
//! 产物是自包含单文件：CSS/JS 内嵌、零外部依赖、离线可用。CSS 原样移植
//! sst/opencode share 组件（starlight-props + custom 覆盖 + share/part/
//! content-*/copy-button 模块，命名空间化），结构与 opencode TSX 输出一致
//! （`data-component`/`data-slot` 属性）。事实源 = `share-page-template.html`
//! v3.0（MD5 09e59e72）；四视图（对话/Team/私聊/频道）数据语义保留 bingo，
//! 视觉走 opencode 令牌。
//!
//! 数据由 Rust 服务端渲染：所有动态文本先经 [`escape`]（`& < > " '` 全量转义）
//! 再拼进 HTML；JS 只做渐进增强（tab/锚点复制/展开/回到顶部/打印），不拼接
//! 任何数据——无脚本注入面。文本块走最小 markdown→HTML（标题/粗体/行内
//! 代码/代码块/列表），不做完整 md 引擎（高亮 P2，纯 `<pre>`）。

use std::collections::HashMap;

use crate::api::types::{ContentBlock, Message, Role};
use crate::share::{AgentShare, ChannelShare, ShareDoc};

/// HTML 转义（属性与文本上下文通用：`&` 先转，防二次转义错位）。
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// 行内格式：`code` 与 **bold**（输入须已转义；code 内容不再二次转义）。
fn inline_bold(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    loop {
        match rest.find("**") {
            None => {
                out.push_str(rest);
                break;
            }
            Some(i) => {
                out.push_str(&rest[..i]);
                let after = &rest[i + 2..];
                match after.find("**") {
                    Some(end) => {
                        out.push_str(&format!("<strong>{}</strong>", &after[..end]));
                        rest = &after[end + 2..];
                    }
                    None => {
                        out.push_str("**");
                        rest = after;
                    }
                }
            }
        }
    }
    out
}

fn inline_md(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    loop {
        match rest.find('`') {
            None => {
                out.push_str(&inline_bold(rest));
                break;
            }
            Some(i) => {
                out.push_str(&inline_bold(&rest[..i]));
                let after = &rest[i + 1..];
                match after.find('`') {
                    Some(end) => {
                        out.push_str(&format!("<code>{}</code>", &after[..end]));
                        rest = &after[end + 1..];
                    }
                    None => {
                        out.push('`');
                        rest = after;
                    }
                }
            }
        }
    }
    out
}

/// 标题行（1-3 级）：`# ` / `## ` / `### `。
fn heading_level(line: &str) -> Option<usize> {
    let trimmed = line.trim_start();
    let hashes = trimmed.chars().take_while(|c| *c == '#').count();
    if (1..=3).contains(&hashes) && trimmed[hashes..].starts_with(' ') {
        Some(hashes)
    } else {
        None
    }
}

/// 列表项：(是否有序, 内容)。无序 `- `/`* `，有序 `N. `。
fn list_item(line: &str) -> Option<(bool, &str)> {
    let trimmed = line.trim_start();
    if let Some(rest) = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
    {
        return Some((false, rest));
    }
    let digits = trimmed.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits > 0 && trimmed[digits..].starts_with(". ") {
        return Some((true, &trimmed[digits + 2..]));
    }
    None
}

/// 最小 markdown → HTML（design.md v3.0 §3.3 安全子集）。逐行渲染，
/// 代码块为纯 `<pre><code>`（cm-root CSS 已内置观感，无 shiki 高亮）。
pub fn render_markdown(text: &str) -> String {
    let mut out = String::new();
    let mut lines = text.lines().peekable();
    let mut in_code = false;
    let mut code_lang = String::new();
    let mut code_buf = String::new();
    let close_fence = |lang: &str, buf: &str, out: &mut String| {
        out.push_str("<pre><code");
        if !lang.is_empty() {
            out.push_str(&format!(" class=\"language-{}\"", escape(lang)));
        }
        out.push('>');
        out.push_str(&escape(buf.trim_end()));
        out.push_str("</code></pre>");
    };
    while let Some(line) = lines.next() {
        if let Some(lang) = line.strip_prefix("```") {
            if in_code {
                close_fence(&code_lang, &code_buf, &mut out);
                in_code = false;
            } else {
                in_code = true;
                code_lang = lang.trim().to_string();
                code_buf.clear();
            }
            continue;
        }
        if in_code {
            code_buf.push_str(line);
            code_buf.push('\n');
            continue;
        }
        if let Some(level) = heading_level(line) {
            let content = inline_md(&escape(line.trim_start_matches('#').trim()));
            out.push_str(&format!("<h{level}>{content}</h{level}>"));
            continue;
        }
        if let Some((ordered, content)) = list_item(line) {
            let tag = if ordered { "ol" } else { "ul" };
            out.push_str(&format!("<{tag}>"));
            out.push_str(&format!("<li>{}</li>", inline_md(&escape(content))));
            while let Some(next) = lines.peek() {
                let Some((ordered_next, content_next)) = list_item(next) else { break };
                if ordered_next != ordered {
                    break;
                }
                out.push_str(&format!("<li>{}</li>", inline_md(&escape(content_next))));
                lines.next();
            }
            out.push_str(&format!("</{tag}>"));
            continue;
        }
        if line.trim().is_empty() {
            continue;
        }
        out.push_str(&format!("<p>{}</p>", inline_md(&escape(line))));
    }
    if in_code {
        close_fence(&code_lang, &code_buf, &mut out);
    }
    out
}

fn tool_result_text(content: &serde_json::Value) -> String {
    match content {
        serde_json::Value::String(s) => s.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_default(),
    }
}

/// 截断到 60 字符（超长加省略号；t-args/目标摘要用）。
fn clip(text: &str) -> String {
    let cut: String = text.chars().take(60).collect();
    if cut.chars().count() < text.chars().count() {
        format!("{cut}…")
    } else {
        cut
    }
}

/// 工具目标参数摘要（tool-title 的 target）：命令/文件路径/pattern 等首值。
fn tool_target(name: &str, input: &serde_json::Value) -> String {
    let picked = match name {
        "Bash" => input.get("command"),
        "Read" | "Write" | "Edit" => input.get("file_path"),
        "Glob" | "Grep" => input.get("pattern"),
        "WebFetch" => input.get("url"),
        "WebSearch" => input.get("query"),
        "Agent" => input.get("prompt").or_else(|| input.get("description")),
        "SendMessage" => input.get("agent"),
        "TaskCreate" => input.get("subject"),
        "TaskUpdate" | "TaskGet" => input.get("task_id"),
        _ => None,
    };
    match picked {
        Some(serde_json::Value::String(s)) if !s.is_empty() => clip(s),
        _ => {
            if let serde_json::Value::Object(map) = input {
                map.values()
                    .find_map(|v| v.as_str())
                    .map(clip)
                    .unwrap_or_default()
            } else {
                String::new()
            }
        }
    }
}

/// tool_use_id → (工具名, 目标摘要)，供 ToolResult 部件还原工具语义
/// （bash 结果走终端窗、read 结果走代码卡、error 走红标）。
fn build_tool_map(messages: &[Message]) -> HashMap<String, (String, String)> {
    let mut map = HashMap::new();
    for msg in messages {
        for block in &msg.content {
            if let ContentBlock::ToolUse { id, name, input } = block {
                map.insert(id.clone(), (name.clone(), tool_target(name, input)));
            }
        }
    }
    map
}

// ── 锚点三态 SVG（角色图标 → hover 变 # → 复制后 ✓；part.tsx/common.tsx）──

const ICON_USER: &str = r#"<svg width="18" height="18" viewBox="0 0 18 18" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"><circle cx="9" cy="7" r="3.2"/><path d="M3.8 14.8a5.2 5.2 0 0 1 10.4 0"/></svg>"#;
const ICON_SPARKLE: &str = r#"<svg width="18" height="18" viewBox="0 0 18 18" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"><path d="M9 3l1.6 3.9 3.9 1.6-3.9 1.6L9 14 7.4 10.1 3.5 8.5l3.9-1.6z"/></svg>"#;
const ICON_THINKING: &str = r#"<svg width="18" height="18" viewBox="0 0 18 18" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"><path d="M9 4v10M4 9h10M5.5 5.5l7 7M12.5 5.5l-7 7"/></svg>"#;
const ICON_TERMINAL: &str = r#"<svg width="18" height="18" viewBox="0 0 18 18" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"><path d="M4 5l4 4-4 4M10 13h4"/></svg>"#;
const ICON_DOC: &str = r#"<svg width="18" height="18" viewBox="0 0 18 18" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"><path d="M6 2.5h6l3 3V15.5a1 1 0 0 1-1 1H6a1 1 0 0 1-1-1V3.5a1 1 0 0 1 1-1z"/><path d="M11 2.5V6h3.5"/></svg>"#;
const ICON_WRITE: &str = r#"<svg width="18" height="18" viewBox="0 0 18 18" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"><path d="M6 2.5h6l3 3V15.5a1 1 0 0 1-1 1H6a1 1 0 0 1-1-1V3.5a1 1 0 0 1 1-1z"/><path d="M11 2.5V6h3.5"/><path d="M9 8v4M7 10h4"/></svg>"#;
const ICON_EDIT: &str = r#"<svg width="18" height="18" viewBox="0 0 18 18" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"><path d="M12.5 2.5 15.5 5.5 6 15H3v-3z"/><path d="M11 4l3 3"/></svg>"#;
const ICON_GREP: &str = r#"<svg width="18" height="18" viewBox="0 0 18 18" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"><path d="M6 2.5h6l3 3V15.5a1 1 0 0 1-1 1H6a1 1 0 0 1-1-1V3.5a1 1 0 0 1 1-1z"/><circle cx="8.5" cy="10" r="1.8"/><path d="M8.5 11.8 7 13.5"/></svg>"#;
const ICON_GLOB: &str = r#"<svg width="18" height="18" viewBox="0 0 18 18" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"><circle cx="7.5" cy="7.5" r="4"/><path d="M10.5 10.5 14 14"/></svg>"#;
const ICON_LIST: &str = r#"<svg width="18" height="18" viewBox="0 0 18 18" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"><rect x="3.5" y="3.5" width="11" height="11" rx="1.5"/><path d="M6.5 7h5M6.5 10h5"/></svg>"#;
const ICON_GLOBE: &str = r#"<svg width="18" height="18" viewBox="0 0 18 18" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"><circle cx="9" cy="9" r="6"/><path d="M3 9h12M9 3a8.5 8.5 0 0 1 0 12M9 3a8.5 8.5 0 0 0 0 12"/></svg>"#;
const ICON_ERROR: &str = r#"<svg width="18" height="18" viewBox="0 0 18 18" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"><rect x="4.5" y="7" width="9" height="7" rx="1.5"/><circle cx="7.5" cy="10.5" r=".8"/><circle cx="10.5" cy="10.5" r=".8"/><path d="M9 4.5V7M9 4.5l-2-1.5M9 4.5l2-1.5"/></svg>"#;
const ICON_IMAGE: &str = r#"<svg width="18" height="18" viewBox="0 0 18 18" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"><rect x="3" y="3.5" width="12" height="11" rx="1.5"/><circle cx="7" cy="7.5" r="1.2"/><path d="M3.5 12.5l3.5-3 3 3 2.5-2.5 2 2"/></svg>"#;
const ICON_HASHTAG: &str = r#"<svg width="18" height="18" viewBox="0 0 18 18" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"><path d="M6.5 3.5 5 14.5M13 3.5 11.5 14.5M3.5 7h11M3.5 11h11"/></svg>"#;
const ICON_CHECK: &str = r#"<svg width="18" height="18" viewBox="0 0 18 18" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"><circle cx="9" cy="9" r="6.5"/><path d="M6 9.2l2 2 4-4.4"/></svg>"#;

/// 工具名 → 锚点首图标。
fn tool_icon(name: &str) -> &'static str {
    match name {
        "Bash" => ICON_TERMINAL,
        "Read" => ICON_DOC,
        "Write" => ICON_WRITE,
        "Edit" => ICON_EDIT,
        "Grep" => ICON_GREP,
        "Glob" => ICON_GLOB,
        "List" => ICON_LIST,
        "WebFetch" | "WebSearch" => ICON_GLOBE,
        _ => ICON_SPARKLE,
    }
}

/// 消息锚点（opencode data-slot="anchor"：首图标 + # + ✓ + Copied tooltip）。
fn anchor(id: &str, icon: &str) -> String {
    format!(
        "<div data-slot=\"anchor\" title=\"Copy link to this message\"><a href=\"#{id}\">{icon}{ICON_HASHTAG}{ICON_CHECK}</a><span data-slot=\"tooltip\">Copied</span></div>"
    )
}

/// 复制按钮（copy-button.tsx：hover 显现，点击复制 2s ✓）。
const COPY_BUTTON: &str = r#"<div data-component="copy-button" class="copy-root"><button type="button" aria-label="Copy" title="Copy"><svg viewBox="0 0 18 18" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"><rect x="5" y="4" width="8" height="11.5" rx="1.5"/><path d="M7.5 2.5h3v2.5h-3z"/></svg></button></div>"#;

/// 展开/收起按钮（opencode ResultsButton：data-more + 图标）。
fn expand_button(label: &str) -> String {
    format!(
        "<button type=\"button\" data-component=\"button-text\" data-more aria-expanded=\"true\"><span>{label}</span><span data-slot=\"icon\"><svg width=\"11\" height=\"11\" viewBox=\"0 0 18 18\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.5\" stroke-linecap=\"round\"><path d=\"M4.5 7 9 11.5 13.5 7\"/></svg></span></button>"
    )
}

/// part 骨架：装饰列（锚点 + 3px 竖线）+ 内容列。
fn part(id: &str, data_type: &str, role: &str, tool: Option<&str>, icon: &str, inner: &str) -> String {
    let tool_attr = tool.map(|t| format!(" data-tool=\"{t}\"")).unwrap_or_default();
    format!(
        "<div class=\"part-root\" data-component=\"part\" data-type=\"{data_type}\" data-role=\"{role}\"{tool_attr} id=\"{id}\"><div data-component=\"decoration\">{}</div><div data-component=\"content\">{inner}</div></div>",
        anchor(id, icon)
    )
}

/// 文本部件：user 无框（content-text），assistant 蓝框卡（content-markdown）。
fn text_part(id: &str, role: Role, text: &str) -> String {
    match role {
        Role::User => part(
            id,
            "text",
            "user",
            None,
            ICON_USER,
            &format!(
                "<div data-component=\"user-text\"><div class=\"ct-root\" data-component=\"content-text\"><pre data-slot=\"text\">{}</pre>{COPY_BUTTON}</div></div>",
                escape(text)
            ),
        ),
        Role::Assistant => part(
            id,
            "text",
            "assistant",
            None,
            ICON_SPARKLE,
            &format!(
                "<div data-component=\"assistant-text\"><div data-component=\"assistant-text-markdown\"><div class=\"cm-root\" data-component=\"content-markdown\" data-expanded><div data-slot=\"markdown\">{}</div>{COPY_BUTTON}</div></div></div>",
                render_markdown(text)
            ),
        ),
    }
}

/// thinking 部件：tool-title（Thinking）+ assistant-reasoning（蓝框小卡）。
fn thinking_part(id: &str, thinking: &str) -> String {
    let markdown = format!(
        "<div data-component=\"assistant-reasoning-markdown\"><div class=\"cm-root\" data-component=\"content-markdown\" data-expanded><div data-slot=\"markdown\">{}</div></div></div>",
        render_markdown(thinking)
    );
    let reasoning = format!(
        "<div data-component=\"assistant-reasoning\">{}{markdown}</div>",
        expand_button("Hide details")
    );
    let inner = format!(
        "<div data-component=\"tool\"><div data-component=\"tool-title\"><span data-slot=\"name\">Thinking</span></div>{reasoning}</div>"
    );
    part(id, "reasoning", "assistant", None, ICON_THINKING, &inner)
}

/// bash 终端窗（content-bash：三点头 Shell 头 + command + output，opencode 原样）。
fn bash_terminal(command: &str, output: &str) -> String {
    let mut content = String::new();
    if !command.is_empty() {
        content.push_str(&format!("<pre>{}</pre>", escape(command)));
    }
    if !output.is_empty() {
        content.push_str(&format!("<div data-slot=\"output\"><pre>{}</pre></div>", escape(output)));
    }
    format!(
        "<div class=\"cb-root\" data-component=\"content-bash\" data-expanded><div data-slot=\"body\"><div data-slot=\"header\"><span>Shell</span></div><div data-slot=\"content\">{content}</div></div>{COPY_BUTTON}</div>"
    )
}

/// Bash 非 command 字段 → tool-args 网格（A4 契约，uiux e79b37aa）：
/// flat 值直出、嵌套值 JSON 序列化，word-break 不截断；command 不进网格。
fn bash_extra_args(input: &serde_json::Value) -> String {
    let Some(obj) = input.as_object() else {
        return String::new();
    };
    let extras: Vec<(&String, &serde_json::Value)> = obj
        .iter()
        .filter(|(k, _)| k.as_str() != "command")
        .collect();
    if extras.is_empty() {
        return String::new();
    }
    let mut out = String::from("<div data-component=\"tool-args\">");
    for (key, value) in extras {
        let text = match value {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        out.push_str(&format!(
            "<div></div><div>{}</div><div>{}</div>",
            escape(key),
            escape(&text)
        ));
    }
    out.push_str("</div>");
    out
}

/// 代码结果块（content-code：纯 pre + copy）。
fn code_result(pre: &str) -> String {
    format!(
        "<div class=\"cc-root\" data-component=\"content-code\"><pre>{}</pre>{COPY_BUTTON}</div>",
        escape(pre)
    )
}

/// 文本结果块（content-text compact：3 行折叠 + copy）。
fn text_result(pre: &str) -> String {
    format!(
        "<div class=\"ct-root\" data-component=\"content-text\" data-compact data-expanded><pre data-slot=\"text\">{}</pre>{COPY_BUTTON}</div>",
        escape(pre)
    )
}

/// 工具标题行（tool-title：name + target）。
fn tool_title(name: &str, target: &str) -> String {
    let target = if target.is_empty() {
        String::new()
    } else {
        format!("<span data-slot=\"target\" title=\"{}\">{}</span>", escape(target), escape(target))
    };
    format!(
        "<div data-component=\"tool-title\"><span data-slot=\"name\">{}</span>{target}</div>",
        escape(name)
    )
}

/// 工具参数网格（tool-args：分隔条/键/值，未知工具 fallback 摘要；
/// 完整 input JSON 仍在 tool-result 块呈现——A4 不丢失）。
fn tool_args(input: &serde_json::Value) -> String {
    let Some(map) = input.as_object() else {
        return String::new();
    };
    let mut out = String::from("<div data-component=\"tool-args\">");
    for (key, value) in map {
        let value = value.as_str().map(clip).unwrap_or_default();
        out.push_str(&format!(
            "<div></div><div>{}</div><div>{}</div>",
            escape(key),
            escape(&value)
        ));
    }
    out.push_str("</div>");
    out
}

/// 工具部件（opencode tool 两段式）。kind = "use"（有 input）| "result"（有 output）。
fn tool_part(
    id: &str,
    tool: &str,
    icon: &str,
    title: String,
    args: String,
    result: String,
) -> String {
    let body = format!(
        "<div data-component=\"tool\" data-tool=\"{}\">{title}{args}{result}</div>",
        escape(tool)
    );
    part(id, "tool", "assistant", Some(tool), icon, &body)
}

/// tool_use 部件：输入侧（bash → 终端窗 command；read/write 等 → 代码卡 input）。
fn tool_use_part(id: &str, name: &str, input: &serde_json::Value) -> String {
    let icon = tool_icon(name);
    let target = tool_target(name, input);
    let pretty = serde_json::to_string_pretty(input).unwrap_or_default();
    let args = if name == "Bash" {
        // A4（uiux e79b37aa 契约）：非 command 字段走 opencode tool-args 网格。
        bash_extra_args(input)
    } else if matches!(
        name,
        "Read" | "Write" | "Edit" | "Grep" | "Glob" | "WebFetch" | "WebSearch"
    ) {
        String::new()
    } else {
        tool_args(input)
    };
    let result = if name == "Bash" {
        let command = target.clone();
        bash_terminal(&command, "")
    } else if args.is_empty() {
        format!(
            "<div data-component=\"tool-result\">{}{}</div>",
            expand_button("Hide input"),
            code_result(&pretty)
        )
    } else {
        format!(
            "<div data-component=\"tool-result\">{}{}</div>",
            expand_button("Hide input"),
            text_result(&pretty)
        )
    };
    tool_part(id, name, icon, tool_title(name, &target), args, result)
}

/// tool_result 部件：输出侧（经 tool_use_id 还原工具名；error → 红标）。
fn tool_result_part(id: &str, tool_use_id: &str, content: &serde_json::Value, is_error: bool, map: &HashMap<String, (String, String)>) -> String {
    let text = tool_result_text(content);
    if is_error {
        let inner = format!(
            "<div data-component=\"tool\" data-tool=\"error\"><div class=\"ce-root\" data-component=\"content-error\" data-expanded><div data-section=\"content\"><pre><span data-color=\"red\" data-marker=\"label\" data-separator>Error</span><span>{}</span></pre></div></div></div>",
            escape(&text)
        );
        return part(id, "tool", "user", Some("error"), ICON_ERROR, &inner);
    }
    let (name, target) = map.get(tool_use_id).cloned().unwrap_or_else(|| ("result".to_string(), String::new()));
    let icon = tool_icon(&name);
    let result = if name == "Bash" {
        bash_terminal(&target, &text)
    } else if matches!(name.as_str(), "Read" | "Write" | "Edit" | "WebFetch" | "WebSearch") {
        format!(
            "<div data-component=\"tool-result\">{}{}</div>",
            expand_button("Hide result"),
            code_result(&text)
        )
    } else {
        format!(
            "<div data-component=\"tool-result\">{}{}</div>",
            expand_button("Hide result"),
            text_result(&text)
        )
    };
    tool_part(id, &name, icon, tool_title(&name, &target), String::new(), result)
}

/// 消息内容块集合 → parts（每块一个 part；id 全局递增）。
fn render_parts(
    messages: &[Message],
    map: &HashMap<String, (String, String)>,
) -> String {
    let mut out = String::new();
    let mut n = 0usize;
    for msg in messages {
        for block in &msg.content {
            n += 1;
            let id = format!("msg-{n}");
            let part_html = match block {
                ContentBlock::Text { text } => match msg.role {
                    Role::User => text_part(&id, Role::User, text),
                    Role::Assistant => text_part(&id, Role::Assistant, text),
                },
                ContentBlock::Thinking { thinking, .. } => thinking_part(&id, thinking),
                ContentBlock::ToolUse { name, input, .. } => tool_use_part(&id, name, input),
                ContentBlock::ToolResult { tool_use_id, content, is_error } => {
                    tool_result_part(&id, tool_use_id, content, *is_error, map)
                }
                ContentBlock::Image { source } => {
                    let media = escape(&source.media_type);
                    let alt = format!("image ({media})");
                    part(
                        &id,
                        "image",
                        if msg.role == Role::User { "user" } else { "assistant" },
                        None,
                        ICON_IMAGE,
                        &format!(
                            "<div style=\"max-width:var(--md-tool-width)\"><img src=\"data:{media};base64,{}\" alt=\"{alt}\"></div>",
                            escape(&source.data)
                        ),
                    )
                }
            };
            out.push_str(&part_html);
        }
    }
    out
}

/// 成员取色（v3.0）：main/user 恒 text-strong，其余按名字 FNV 哈希取 hue-0..5。
fn member_color(name: &str) -> &'static str {
    match name {
        "main" | "assistant" | "user" => "var(--color-text-strong)",
        _ => {
            let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
            for b in name.bytes() {
                hash ^= u64::from(b);
                hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            }
            const HUES: [&str; 6] = [
                "var(--hue-0)",
                "var(--hue-1)",
                "var(--hue-2)",
                "var(--hue-3)",
                "var(--hue-4)",
                "var(--hue-5)",
            ];
            HUES[(hash % 6) as usize]
        }
    }
}

/// 状态字形：idle ● / running ◐ / stopped ✗。
fn state_glyph(state: &str) -> &'static str {
    match state {
        "running" => "◐",
        "stopped" => "✗",
        _ => "●",
    }
}

/// HTML id 安全化（team/dm/channel 锚点用；实例名已接近 slug，兜底替换）。
fn id_slug(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect();
    if cleaned.is_empty() {
        "agent".to_string()
    } else {
        cleaned
    }
}

/// 彩色圆点锚点图标（Team 线程/DM/频道行首；style 注入成员色）。
fn dot_icon(color: &str) -> String {
    format!(
        "<svg width=\"18\" height=\"18\" viewBox=\"0 0 18 18\" style=\"color:{color}\"><circle cx=\"9\" cy=\"9\" r=\"5.5\" fill=\"currentColor\"/></svg>"
    )
}

/// 消息首段文本（Team 线程预览 / DM 行文本用）。
fn first_text(messages: &[Message]) -> String {
    for m in messages.iter().rev() {
        for block in &m.content {
            if let ContentBlock::Text { text } = block
                && !text.trim().is_empty()
            {
                return text.clone();
            }
        }
    }
    String::new()
}

/// Team 视图：线程列表（聊天应用会话列表心智，点击/锚点直达私聊 #dm-<agent>）。
fn render_team(agents: &[AgentShare]) -> String {
    if agents.is_empty() {
        return "<div class=\"view-empty\">— No agents —</div>".to_string();
    }
    let mut rows = String::new();
    for a in agents {
        let slug = id_slug(&a.name);
        let color = member_color(&a.name);
        let def = a.def.as_deref().map(escape).unwrap_or_default();
        let preview = first_text(&a.history);
        let preview_html = if preview.is_empty() {
            "<p class=\"view-empty\">(no history yet)</p>".to_string()
        } else {
            format!(
                "<div class=\"ct-root\" data-component=\"content-text\" data-compact><pre data-slot=\"text\">{}</pre></div>",
                escape(&preview)
            )
        };
        rows.push_str(&format!(
            "<div class=\"part-root thread-row\" data-component=\"part\" data-type=\"thread\" data-agent=\"{}\" id=\"team-{}\" data-jump=\"#dm-{}\" tabindex=\"0\" role=\"link\" aria-label=\"Open {} thread\"><div data-component=\"decoration\"><div data-slot=\"anchor\" title=\"Copy link to this message\"><a href=\"#dm-{}\">{}{}{}</a><span data-slot=\"tooltip\">Copied</span></div><div data-slot=\"bar\"></div></div><div data-component=\"content\"><div data-component=\"step-start\"><div data-slot=\"provider\" style=\"color:{}\">{}</div><div data-slot=\"model\">{} {} · {} · {} messages</div></div>{preview_html}<div data-component=\"content-footer\">last message · {} messages</div></div></div>",
            escape(&a.name),
            slug,
            slug,
            escape(&a.name),
            slug,
            dot_icon(color),
            ICON_HASHTAG,
            ICON_CHECK,
            color,
            escape(&a.name),
            state_glyph(&a.state),
            escape(&a.state),
            def,
            a.history.len(),
            a.history.len()
        ));
    }
    format!("<div class=\"thread-list\">{rows}</div>")
}

/// 私聊视图：每子代理一个 thread part，历史为 dm-msg 聊天流（user 靠右）。
fn render_private(agents: &[AgentShare]) -> String {
    if agents.is_empty() {
        return "<div class=\"view-empty\">— No agents —</div>".to_string();
    }
    let mut out = String::from("<div class=\"view-block\">");
    for a in agents {
        let slug = id_slug(&a.name);
        let color = member_color(&a.name);
        let def = a.def.as_deref().map(escape).unwrap_or_default();
        let mut thread = String::new();
        if a.history.is_empty() {
            thread.push_str("<div class=\"dm-thread\"><p class=\"view-empty\">(no history yet)</p></div>");
        } else {
            thread.push_str("<div class=\"dm-thread\">");
            for m in &a.history {
                let (from, user_cls, from_color) = match m.role {
                    Role::User => ("user", " dm-user", "var(--color-text-strong)"),
                    Role::Assistant => (a.name.as_str(), "", ""),
                };
                let from_color = if from_color.is_empty() {
                    color
                } else {
                    from_color
                };
                let text = first_text(std::slice::from_ref(m));
                if text.is_empty() {
                    continue;
                }
                thread.push_str(&format!(
                    "<div class=\"dm-msg{user_cls}\"><div data-component=\"tool-title\"><span data-slot=\"name\" style=\"color:{from_color}\">{}</span></div><div class=\"ct-root\" data-component=\"content-text\" data-expanded><pre data-slot=\"text\">{}</pre></div></div>",
                    escape(from),
                    escape(&text)
                ));
            }
            thread.push_str("</div>");
        }
        out.push_str(&format!(
            "<div class=\"part-root\" data-component=\"part\" data-type=\"thread\" data-role=\"assistant\" data-agent=\"{}\" id=\"dm-{}\"><div data-component=\"decoration\"><div data-slot=\"anchor\" title=\"Copy link to this message\"><a href=\"#dm-{}\">{}{}{}</a><span data-slot=\"tooltip\">Copied</span></div><div data-slot=\"bar\"></div></div><div data-component=\"content\"><div data-component=\"step-start\"><div data-slot=\"provider\" style=\"color:{}\">{}</div><div data-slot=\"model\">{} {} · {def}</div></div>{thread}</div></div>",
            escape(&a.name),
            slug,
            slug,
            dot_icon(color),
            ICON_HASHTAG,
            ICON_CHECK,
            color,
            escape(&a.name),
            state_glyph(&a.state),
            escape(&a.state)
        ));
    }
    out.push_str("</div>");
    out
}

/// 频道视图：每频道一个聊天记录流（part 消息，seq/成员徽标保留）。
fn render_channels(channels: &[ChannelShare]) -> String {
    if channels.is_empty() {
        return "<div class=\"view-empty\">— No channels —</div>".to_string();
    }
    let mut out = String::from("<div class=\"view-block\">");
    for c in channels {
        let slug = id_slug(&c.name);
        let chips: String = c
            .members
            .iter()
            .map(|m| {
                format!(
                    "<span class=\"m-chip\" style=\"--chip:{}\">{}</span>",
                    member_color(m),
                    escape(m)
                )
            })
            .collect();
        let mut stream = String::new();
        if c.messages.is_empty() {
            stream.push_str("<p class=\"view-empty\">(no messages yet)</p>");
        } else {
            stream.push_str("<div class=\"ch-stream\">");
            let mut n = 0usize;
            for m in &c.messages {
                n += 1;
                let color = member_color(&m.from);
                let user_cls = if m.from == "user" { " dm-user" } else { "" };
                stream.push_str(&format!(
                    "<div class=\"part-root{user_cls}\" data-component=\"part\" data-type=\"text\" data-role=\"assistant\" data-from=\"{}\" id=\"ch-{}\"><div data-component=\"decoration\"><div data-slot=\"anchor\" title=\"Copy link to this message\"><a href=\"#ch-{}\">{}{}{}</a><span data-slot=\"tooltip\">Copied</span></div><div data-slot=\"bar\"></div></div><div data-component=\"content\"><div data-component=\"tool-title\"><span data-slot=\"name\" style=\"color:{}\">{}</span><span data-slot=\"target\" class=\"ch-row-seq\">#{:04}</span></div><div class=\"ct-root\" data-component=\"content-text\" data-expanded><pre data-slot=\"text\">{}</pre></div></div></div>",
                    escape(&m.from),
                    n,
                    n,
                    dot_icon(color),
                    ICON_HASHTAG,
                    ICON_CHECK,
                    color,
                    escape(&m.from),
                    m.seq,
                    escape(&m.text)
                ));
            }
            stream.push_str("</div>");
        }
        out.push_str(&format!(
            "<section class=\"ch-block\" data-component=\"channel\" id=\"channel-{slug}\"><header class=\"ch-head\" data-component=\"step-start\"><div data-slot=\"provider\">◇ #{}</div><div data-slot=\"model\"><span class=\"ch-mode {}\">{}</span><span class=\"ch-members\">{chips}</span></div></header>{stream}</section>",
            escape(&c.name),
            escape(&c.mode),
            escape(&c.mode)
        ));
    }
    out.push_str("</div>");
    out
}

/// epoch 秒 → "Mon D, YYYY HH:MM UTC"（无 chrono 依赖，civil-from-days 算法）。
fn format_epoch(secs: u64) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let days = (secs / 86_400) as i64;
    let secs_of_day = secs % 86_400;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    let h = secs_of_day / 3600;
    let min = (secs_of_day % 3600) / 60;
    format!("{} {}, {} {:02}:{:02} UTC", MONTHS[(m - 1) as usize], d, y, h, min)
}

/// 生成自包含 HTML 文档（conversation 来自主 transcript，其余来自 ShareDoc）。
pub fn render(doc: &ShareDoc, messages: &[Message]) -> String {
    let session = escape(&doc.session);
    let created = format_epoch(doc.created_at);
    let tool_map = build_tool_map(messages);
    let parts = render_parts(messages, &tool_map);

    let mut out = String::new();
    out.push_str("<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n");
    out.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    out.push_str("<meta name=\"generator\" content=\"bingo share\">\n");
    out.push_str("<meta name=\"color-scheme\" content=\"light dark\">\n");
    out.push_str(&format!("<title>bingo · {session}</title>\n"));
    out.push_str("<style>\n");
    out.push_str(CSS);
    out.push_str("</style>\n</head>\n<body>\n");
    out.push_str("<div class=\"share-root\" data-component=\"share\">\n");
    // 头部：header-title + header-stats + header-time。
    out.push_str("<header data-component=\"header\">");
    out.push_str(&format!("<h1 data-component=\"header-title\">{session}</h1>"));
    out.push_str("<div data-component=\"header-details\"><ul data-component=\"header-stats\">");
    out.push_str(&format!(
        "<li data-slot=\"item\"><span data-slot=\"icon\"><svg width=\"16\" height=\"16\" viewBox=\"0 0 18 18\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.5\" stroke-linecap=\"round\"><rect x=\"3.5\" y=\"4.5\" width=\"11\" height=\"10\" rx=\"1.5\"/><path d=\"M3.5 7.5h11M6.5 2.5v3M11.5 2.5v3\"/></svg></span><span data-placeholder>started</span><span>{created}</span></li>"
    ));
    out.push_str("<li data-slot=\"item\"><span data-slot=\"icon\"><svg width=\"16\" height=\"16\" viewBox=\"0 0 18 18\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.5\" stroke-linecap=\"round\"><path d=\"M3 5.5a1 1 0 0 1 1-1h3.2l1.6 2H14a1 1 0 0 1 1 1v7a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1z\"/></svg></span><span>bingo</span></li>");
    out.push_str("</ul><div data-component=\"header-time\">");
    out.push_str(&created);
    out.push_str("</div></div></header>\n");
    // 四视图 tab 导航。
    out.push_str("<nav data-component=\"tabs\" role=\"tablist\" aria-label=\"Views\">");
    out.push_str("<button role=\"tab\" data-tab=\"conv\" aria-selected=\"true\">Conversation</button>");
    out.push_str(&format!("<button role=\"tab\" data-tab=\"team\" aria-selected=\"false\">Team <span class=\"tab-count\">{}</span></button>", doc.agents.len()));
    out.push_str(&format!("<button role=\"tab\" data-tab=\"dm\" aria-selected=\"false\">DM <span class=\"tab-count\">{}</span></button>", doc.agents.len()));
    out.push_str(&format!("<button role=\"tab\" data-tab=\"channel\" aria-selected=\"false\">Channels <span class=\"tab-count\">{}</span></button>", doc.channels.len()));
    out.push_str("</nav>\n");
    // ① 对话：opencode parts 流（追加 view class 修正模板 tab 切换对 conv 的显隐）。
    let conv_inner = if parts.is_empty() {
        "<div class=\"view-empty\">— No messages —</div>".to_string()
    } else {
        parts
    };
    out.push_str(&format!(
        "<div class=\"parts view\" data-view=\"conv\" data-component=\"parts\">{conv_inner}</div>\n"
    ));
    // ② Team 名册。
    out.push_str(&format!(
        "<section class=\"view\" data-view=\"team\" hidden aria-label=\"Team\"><h2 data-component=\"view-title\">Team <span class=\"view-count\">· {} instances</span></h2>{}</section>\n",
        doc.agents.len(),
        render_team(&doc.agents)
    ));
    // ③ DM 私聊。
    out.push_str(&format!(
        "<section class=\"view\" data-view=\"dm\" hidden aria-label=\"Direct messages\"><h2 data-component=\"view-title\">Direct Messages <span class=\"view-count\">· {} agents</span></h2>{}</section>\n",
        doc.agents.len(),
        render_private(&doc.agents)
    ));
    // ④ 频道。
    out.push_str(&format!(
        "<section class=\"view\" data-view=\"channel\" hidden aria-label=\"Channels\"><h2 data-component=\"view-title\">Channels <span class=\"view-count\">· {} rooms</span></h2>{}</section>\n",
        doc.channels.len(),
        render_channels(&doc.channels)
    ));
    out.push_str("</div>\n");
    // 回到顶部 + noscript + JS。
    out.push_str("<button type=\"button\" class=\"scroll-button\" data-component=\"scroll\" data-hidden=\"true\" aria-label=\"Scroll to top\"><svg width=\"18\" height=\"18\" viewBox=\"0 0 18 18\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.5\" stroke-linecap=\"round\"><path d=\"M4.5 11.5 9 7l4.5 4.5\"/></svg></button>\n");
    out.push_str("<noscript><div style=\"padding:1.5rem;text-align:center;color:var(--sl-color-text-dimmed)\">This page is fully readable without JavaScript; only copy links, copy buttons and tab switching are enhanced.</div></noscript>\n");
    out.push_str("<script>\n");
    out.push_str(JS);
    out.push_str("</script>\n</body>\n</html>\n");
    out
}

/// 内嵌样式：与 share-page-template.html v3.0（MD5 09e59e72）原样一致。
/// opencode share 组件移植（starlight-props + custom 覆盖 + share/part/
/// content-*/copy-button，命名空间化 .root → .share-root/.part-root/…）。
const CSS: &str = include_str!("../notes/design/share-page-template.css");

/// 渐进增强 JS（与模板同源）：四视图 tab、锚点复制、复制按钮、展开/收起、
/// 回到顶部、打印展开。不拼接任何会话数据（防注入）。
const JS: &str = r#"
(function(){
  'use strict';

  function activateTab(name){
    var views = document.querySelectorAll('.view[data-view]');
    for (var i = 0; i < views.length; i++){
      views[i].hidden = views[i].getAttribute('data-view') !== name;
    }
    var tabs = document.querySelectorAll('[data-component="tabs"] button[data-tab]');
    for (var j = 0; j < tabs.length; j++){
      var on = tabs[j].getAttribute('data-tab') === name;
      tabs[j].setAttribute('aria-selected', on ? 'true' : 'false');
      tabs[j].tabIndex = on ? 0 : -1;
    }
    if (history.replaceState) history.replaceState(null, '', '#' + name);
  }
  function bindTabs(){
    var tabs = Array.prototype.slice.call(document.querySelectorAll('[data-component="tabs"] button[data-tab]'));
    tabs.forEach(function(btn){
      btn.addEventListener('click', function(){ activateTab(btn.getAttribute('data-tab')); });
      btn.addEventListener('keydown', function(e){
        var idx = tabs.indexOf(btn), next;
        if (e.key === 'ArrowRight' || e.key === 'ArrowDown'){
          e.preventDefault(); next = tabs[(idx + 1) % tabs.length];
        } else if (e.key === 'ArrowLeft' || e.key === 'ArrowUp'){
          e.preventDefault(); next = tabs[(idx - 1 + tabs.length) % tabs.length];
        } else { return; }
        next.focus();
        activateTab(next.getAttribute('data-tab'));
      });
    });
    document.addEventListener('keydown', function(e){
      if (e.altKey || e.ctrlKey || e.metaKey) return;
      var map = { '1': 'conv', '2': 'team', '3': 'dm', '4': 'channel' };
      if (map[e.key]) activateTab(map[e.key]);
    });
    var name = (location.hash || '').replace('#', '');
    if (['conv', 'team', 'dm', 'channel'].indexOf(name) >= 0) activateTab(name);
  }

  function bindAnchors(){
    document.addEventListener('click', function(e){
      var anchor = e.target && e.target.closest ? e.target.closest('[data-slot="anchor"] a') : null;
      if (!anchor) return;
      e.preventDefault();
      var hash = anchor.getAttribute('href') || '';
      var url = location.href.split('#')[0] + hash;
      function fallback(){
        var ta = document.createElement('textarea');
        ta.value = url;
        ta.style.position = 'fixed';
        ta.style.opacity = '0';
        document.body.appendChild(ta);
        ta.select();
        try { document.execCommand('copy'); } catch (err) {}
        document.body.removeChild(ta);
      }
      if (navigator.clipboard && navigator.clipboard.writeText){
        navigator.clipboard.writeText(url).catch(function(){ fallback(); });
      } else { fallback(); }
      var slot = anchor.parentElement;
      slot.setAttribute('data-status', 'copied');
      setTimeout(function(){ slot.removeAttribute('data-status'); }, 3000);
    });
  }

  function bindCopyButtons(){
    document.addEventListener('click', function(e){
      var btn = e.target && e.target.closest ? e.target.closest('.copy-root button') : null;
      if (!btn) return;
      var root = btn.closest('.copy-root');
      var container = root && root.parentElement;
      var pre = container ? container.querySelector('pre') : null;
      var text = pre ? pre.textContent : '';
      if (!text) return;
      function fallback(){
        var ta = document.createElement('textarea');
        ta.value = text;
        ta.style.position = 'fixed';
        ta.style.opacity = '0';
        document.body.appendChild(ta);
        ta.select();
        try { document.execCommand('copy'); } catch (err) {}
        document.body.removeChild(ta);
      }
      if (navigator.clipboard && navigator.clipboard.writeText){
        navigator.clipboard.writeText(text).catch(function(){ fallback(); });
      } else { fallback(); }
      btn.setAttribute('data-copied', 'true');
      btn.setAttribute('aria-label', 'Copied');
      btn.setAttribute('title', 'Copied');
      btn.innerHTML = '<svg viewBox="0 0 18 18" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"><circle cx="9" cy="9" r="6.5"/><path d="M6 9.2l2 2 4-4.4"/></svg>';
      setTimeout(function(){
        btn.removeAttribute('data-copied');
        btn.setAttribute('aria-label', 'Copy');
        btn.setAttribute('title', 'Copy');
        btn.innerHTML = '<svg viewBox="0 0 18 18" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"><rect x="5" y="4" width="8" height="11.5" rx="1.5"/><path d="M7.5 2.5h3v2.5h-3z"/></svg>';
      }, 2000);
    });
  }

  function bindToggles(){
    document.addEventListener('click', function(e){
      var btn = e.target && e.target.closest ? e.target.closest('[data-component="button-text"][data-more]') : null;
      if (!btn) return;
      e.preventDefault();
      var expanded = btn.getAttribute('aria-expanded') === 'true';
      btn.setAttribute('aria-expanded', expanded ? 'false' : 'true');
      var label = btn.querySelector('span');
      var root = btn.parentElement;
      var was = expanded;
      if (label) {
        var name = label.textContent || '';
        label.textContent = was ? (name.replace(/^Hide /, 'Show ')) : (name.replace(/^Show /, 'Hide '));
      }
      var targets = root.querySelectorAll('[data-component="tool-result"] > :not(button), [data-component="assistant-reasoning"] > [data-component="assistant-reasoning-markdown"]');
      for (var i = 0; i < targets.length; i++){
        if (targets[i].hasAttribute('data-expanded')) {
          targets[i].setAttribute('data-expanded', was ? 'false' : 'true');
        } else {
          targets[i].hidden = was;
        }
      }
      var icon = btn.querySelector('[data-slot="icon"] svg');
      if (icon) {
        icon.setAttribute('d', was
          ? 'M4.5 7 9 11.5 13.5 7'
          : 'M7 4.5 11.5 9 7 13.5');
      }
    });
  }

  function bindScrollButton(){
    var btn = document.querySelector('[data-component="scroll"]');
    if (!btn) return;
    window.addEventListener('scroll', function(){
      var top = window.scrollY || document.documentElement.scrollTop;
      btn.setAttribute('data-hidden', top < 200 ? 'true' : 'false');
    }, { passive: true });
    btn.addEventListener('click', function(){
      window.scrollTo({ top: 0, behavior: 'smooth' });
    });
  }

  function bindPrint(){
    var saved = [];
    window.addEventListener('beforeprint', function(){
      saved = [];
      var ds = document.querySelectorAll('details');
      for (var i = 0; i < ds.length; i++){ saved.push(ds[i].open); ds[i].open = true; }
      var views = document.querySelectorAll('.view[data-view]');
      for (var j = 0; j < views.length; j++){ views[j].hidden = false; }
    });
    window.addEventListener('afterprint', function(){
      var ds = document.querySelectorAll('details');
      for (var k = 0; k < ds.length; k++){
        if (saved[k] !== undefined) ds[k].open = saved[k];
      }
      var name = (location.hash || '').replace('#', '');
      if (['conv', 'team', 'dm', 'channel'].indexOf(name) >= 0) activateTab(name);
      else activateTab('conv');
    });
  }

  /* ---------- Team 线程行：点击直达私聊（data-jump；锚点内点击交给 bindAnchors） ---------- */
  function bindThreadRows(){
    document.addEventListener('click', function(e){
      var row = e.target && e.target.closest ? e.target.closest('.thread-row[data-jump]') : null;
      if (!row) return;
      if (e.target.closest('[data-slot="anchor"]')) return;
      e.preventDefault();
      var target = row.getAttribute('data-jump') || '';
      var view = target.indexOf('dm') > -1 ? 'dm' : 'conv';
      activateTab(view);
      setTimeout(function(){
        var el = document.querySelector(target);
        if (el) el.scrollIntoView({ behavior: 'smooth', block: 'start' });
      }, 50);
    });
    document.addEventListener('keydown', function(e){
      if (e.key !== 'Enter') return;
      var row = e.target && e.target.closest ? e.target.closest('.thread-row[data-jump]') : null;
      if (row){ e.preventDefault(); row.click(); }
    });
  }

  bindTabs();
  bindAnchors();
  bindCopyButtons();
  bindToggles();
  bindThreadRows();
  bindScrollButton();
  bindPrint();
})();
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::share::{AgentShare, ChannelShare, ShareDoc};

    fn text_msg(role: Role, text: &str) -> Message {
        Message {
            role,
            content: vec![ContentBlock::Text { text: text.into() }],
        }
    }

    /// 富消息：thinking + tool_use(bash) + tool_result(错误) + text。
    fn rich_messages() -> Vec<Message> {
        vec![
            Message {
                role: Role::User,
                content: vec![ContentBlock::Text { text: "你好".into() }],
            },
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::Thinking {
                        thinking: "深入思考一下".into(),
                        signature: "sig".into(),
                    },
                    ContentBlock::ToolUse {
                        id: "tu_1".into(),
                        name: "Bash".into(),
                        input: serde_json::json!({"command": "ls <unsafe> & echo \"x\""}),
                    },
                    ContentBlock::ToolResult {
                        tool_use_id: "tu_1".into(),
                        content: serde_json::Value::String("src/ share.rs".into()),
                        is_error: false,
                    },
                    ContentBlock::Text { text: "**完成**了，`OK`".into() },
                ],
            },
        ]
    }

    fn doc() -> ShareDoc {
        ShareDoc {
            session: "proj-1700000000".into(),
            created_at: 1_700_000_000,
            agents: vec![AgentShare {
                name: "scout".into(),
                def: Some("scout".into()),
                description: "调研".into(),
                state: "running".into(),
                history: vec![
                    text_msg(Role::User, "查一下"),
                    text_msg(Role::Assistant, "**结论**：`ok`"),
                ],
            }],
            channels: vec![ChannelShare {
                name: "table".into(),
                mode: "free".into(),
                members: vec!["main".into(), "user".into(), "scout".into()],
                messages: vec![crate::channels::ChannelMessage {
                    seq: 1,
                    from: "scout".into(),
                    text: "大家好".into(),
                }],
            }],
        }
    }

    #[test]
    fn escapes_html_special_chars() {
        assert_eq!(
            escape("<script>alert('xss')</script> & \"quoted\""),
            "&lt;script&gt;alert(&#39;xss&#39;)&lt;/script&gt; &amp; &quot;quoted&quot;"
        );
        // 注入内容经 render 全链路不出现可执行脚本/未转义标签（C1 全字符集）。
        let html = render(
            &doc(),
            &[text_msg(
                Role::User,
                "<script>alert(1)</script><img src=x onerror=alert(2)>&\"'",
            )],
        );
        assert!(!html.contains("<script>alert(1)"), "注入脚本不得原样出现");
        assert!(!html.contains("<img src=x onerror"), "注入 img 标签不得原样出现");
        assert!(html.contains("&lt;script&gt;alert(1)"));
        assert!(html.contains("&lt;img src=x onerror"));
        assert!(html.contains("&amp;"));
        assert!(html.contains("&quot;"));
        assert!(html.contains("&#39;"));
    }

    #[test]
    fn markdown_headings_bold_code_lists_and_fences() {
        let md = "# 标题\n## 二级\n### 三级\n\n**粗体** 与 `code`\n\n- a\n- b\n\n1. 一\n2. 二\n\n```rust\nfn main() { println!(\"<hi>\"); }\n```\n";
        let html = render_markdown(md);
        assert!(html.contains("<h1>标题</h1>"), "{html}");
        assert!(html.contains("<h2>二级</h2>"));
        assert!(html.contains("<h3>三级</h3>"));
        assert!(html.contains("<strong>粗体</strong>"));
        assert!(html.contains("<code>code</code>"));
        assert!(html.contains("<ul><li>a</li><li>b</li></ul>"));
        assert!(html.contains("<ol><li>一</li><li>二</li></ol>"));
        assert!(html.contains("<pre><code class=\"language-rust\">"), "代码块纯 pre");
        assert!(html.contains("&lt;hi&gt;"), "代码块内容转义");
        assert!(html.contains("println"));
    }

    #[test]
    fn markdown_escapes_before_formatting() {
        let html = render_markdown("**<b>粗</b>** 与 `<i>x</i>`");
        assert!(html.contains("<strong>&lt;b&gt;粗&lt;/b&gt;</strong>"), "{html}");
        assert!(html.contains("<code>&lt;i&gt;x&lt;/i&gt;</code>"), "行内代码内容转义");
    }

    #[test]
    fn markdown_unclosed_fence_renders_safely() {
        let html = render_markdown("```\nno close\n");
        assert!(html.contains("<pre><code>no close</code></pre>"), "{html}");
    }

    #[test]
    fn member_colors_are_stable_and_consistent() {
        assert_eq!(member_color("main"), "var(--color-text-strong)");
        assert_eq!(member_color("user"), "var(--color-text-strong)");
        assert_eq!(member_color("scout"), member_color("scout"));
        assert!(member_color("scout").starts_with("var(--hue-"));
        let colors: std::collections::HashSet<&str> =
            ["dev", "qa", "ui-ux", "main", "scout", "worker"]
                .into_iter()
                .map(member_color)
                .collect();
        assert!(colors.len() >= 4, "成员色应分散：{colors:?}");
    }

    #[test]
    fn renders_all_four_views_with_opencode_structure() {
        let html = render(&doc(), &rich_messages());
        // 四视图 data-view + share-root 骨架。
        for view in ["data-view=\"conv\"", "data-view=\"team\"", "data-view=\"dm\"", "data-view=\"channel\""] {
            assert!(html.contains(view), "缺视图 {view}");
        }
        assert!(html.contains("data-component=\"share\""));
        assert!(html.contains("data-component=\"header\""));
        assert!(html.contains("data-component=\"header-title\""));
        assert!(html.contains("data-component=\"header-stats\""));
        assert!(html.contains("data-component=\"tabs\""));
        // part 骨架：装饰列 + 内容列 + 锚点三态。
        assert!(html.contains("class=\"part-root\""));
        assert!(html.contains("data-component=\"decoration\""));
        assert!(html.contains("data-slot=\"anchor\""));
        assert!(html.contains("data-slot=\"bar\""));
        assert!(html.contains("data-slot=\"tooltip\""));
        assert!(html.contains("data-component=\"content\""));
        assert!(html.contains("id=\"msg-1\""));
        // user 无框 content-text / assistant 蓝框 content-markdown。
        assert!(html.contains("data-component=\"user-text\""));
        assert!(html.contains("data-component=\"assistant-text\""));
        assert!(html.contains("data-component=\"assistant-text-markdown\""));
        assert!(html.contains("data-slot=\"markdown\""));
        assert!(html.contains("class=\"cm-root\"") && html.contains("class=\"ct-root\""));
        // Team 线程列表（thread-row + data-jump 直达私聊）。
        assert!(html.contains("class=\"thread-list\""));
        assert!(html.contains("class=\"part-root thread-row\""));
        assert!(html.contains("data-jump=\"#dm-scout\""));
        assert!(html.contains("href=\"#dm-scout\""));
        assert!(html.contains("id=\"team-scout\""));
        assert!(html.contains("data-slot=\"provider\""));
        assert!(html.contains("2 messages"));
        // DM 私聊（dm-msg 聊天流 + user 靠右；文本为纯 pre 不渲染 markdown）。
        assert!(html.contains("data-type=\"thread\""));
        assert!(html.contains("id=\"dm-scout\""));
        assert!(html.contains("class=\"dm-msg\""));
        assert!(html.contains("class=\"dm-msg dm-user\""));
        assert!(!html.contains("<strong>结论</strong>"), "dm 文本为纯 pre（不做 markdown）");
        // 频道（part 消息流 + seq/成员徽标）。
        assert!(html.contains("class=\"ch-block\"") && html.contains("data-component=\"channel\""));
        assert!(html.contains("<div data-slot=\"provider\">◇ #table</div>"));
        assert!(html.contains("class=\"ch-mode free\""));
        assert!(html.contains("class=\"ch-row-seq\">#0001</span>"));
        assert!(html.contains("class=\"m-chip\""));
        assert!(html.contains("大家好"));
    }

    #[test]
    fn thinking_and_tools_use_opencode_components() {
        let html = render(&doc(), &rich_messages());
        // thinking：tool-title Thinking + assistant-reasoning。
        assert!(html.contains("data-type=\"reasoning\""));
        assert!(html.contains("<span data-slot=\"name\">Thinking</span>"));
        assert!(html.contains("data-component=\"assistant-reasoning\""));
        assert!(html.contains("data-component=\"assistant-reasoning-markdown\""));
        assert!(html.contains("深入思考一下"));
        // bash：终端窗 Shell 头。
        assert!(html.contains("data-tool=\"bash\""));
        assert!(html.contains("data-slot=\"header\"><span>Shell</span>"));
        assert!(html.contains("class=\"cb-root\""));
        assert!(html.contains("ls &lt;unsafe&gt; &amp; echo &quot;x&quot;"));
        // tool_result：还原 bash 语义 → 终端窗含输出。
        assert!(html.contains("<div data-slot=\"output\"><pre>src/ share.rs</pre></div>"));
        // 工具两段式：tool-title + tool-result + 展开按钮。
        assert!(html.contains("data-component=\"tool-title\""));
        assert!(html.contains("data-component=\"tool-result\""));
        assert!(html.contains("data-component=\"button-text\" data-more"));
        // 复制按钮。
        assert!(html.contains("data-component=\"copy-button\""));
        assert!(html.contains("class=\"copy-root\""));
    }

    #[test]
    fn tool_error_uses_red_label() {
        let msgs = vec![Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "tu_x".into(),
                content: serde_json::Value::String("boom".into()),
                is_error: true,
            }],
        }];
        let html = render(&doc(), &msgs);
        assert!(html.contains("data-tool=\"error\""));
        assert!(html.contains("class=\"ce-root\""));
        assert!(html.contains("<span data-color=\"red\" data-marker=\"label\" data-separator>Error</span>"));
        assert!(html.contains("boom"));
    }

    #[test]
    fn bash_input_extra_fields_are_not_lost() {
        // A4 回归（pm #27 + uiux e79b37aa 契约）：Bash input 多字段时
        // 其余字段以 tool-args 网格呈现（flat 直出/嵌套 JSON 序列化），
        // 注入串实体化不丢失。
        let msgs = vec![Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "tu_1".into(),
                name: "Bash".into(),
                input: serde_json::json!({
                    "command": "ls <unsafe> & echo \"x\"",
                    "background": true,
                    "timeout": 1,
                    "evil": "<img src=x onerror=alert(3)>",
                    "nested": {"a": 1}
                }),
            }],
        }];
        let html = render(&doc(), &msgs);
        assert!(html.contains("<pre>ls &lt;unsafe&gt; &amp; echo &quot;x&quot;</pre>"), "command 在 Shell 窗");
        // tool-args 网格：非 command 字段逐键呈现，command 不进网格。
        assert!(html.contains("<div data-component=\"tool-args\">"), "多余字段走 tool-args 网格");
        assert!(html.contains(">background</div><div>true</div>"), "flat 值直出");
        assert!(html.contains(">timeout</div><div>1</div>"));
        assert!(html.contains(">evil</div><div>&lt;img src=x onerror=alert(3)&gt;</div>"), "注入串实体化");
        assert!(html.contains(">nested</div><div>"), "嵌套值序列化");
        assert!(!html.contains("<img src=x onerror"), "无未转义标签");
        // 仅 command 时无冗余网格。
        let msgs = vec![Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "tu_1".into(),
                name: "Bash".into(),
                input: serde_json::json!({"command": "ls"}),
            }],
        }];
        let html = render(&doc(), &msgs);
        assert_eq!(html.matches("<pre>ls</pre>").count(), 1, "仅 command 一个 pre");
        assert!(!html.contains("<div data-component=\"tool-args\">"), "无额外字段省略网格");
    }

    #[test]
    fn unknown_tool_gets_tool_args_grid() {
        let msgs = vec![Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "tu_1".into(),
                name: "Agent".into(),
                input: serde_json::json!({"name": "dev", "description": "Implement share"}),
            }],
        }];
        let html = render(&doc(), &msgs);
        assert!(html.contains("data-component=\"tool-args\""));
        assert!(html.contains(">name</div><div>dev</div>"));
        assert!(html.contains(">description</div><div>Implement share</div>"));
    }

    #[test]
    fn image_inlines_as_data_uri() {
        let msgs = vec![Message {
            role: Role::User,
            content: vec![ContentBlock::Image {
                source: crate::api::types::ImageSource::base64("image/png", "aGVsbG8="),
            }],
        }];
        let html = render(&doc(), &msgs);
        assert!(html.contains("data-type=\"image\""));
        assert!(html.contains("src=\"data:image/png;base64,aGVsbG8=\""));
        assert!(html.contains("alt=\"image (image/png)\""));
        assert!(!html.contains("http://") && !html.contains("https://"), "仅 data: URI");
    }

    #[test]
    fn empty_views_show_hints() {
        let html = render(&ShareDoc::new("s".into()), &[]);
        let empty_count = html.matches("— No ").count();
        assert_eq!(empty_count, 4, "四视图空态：{html}");
        assert!(html.contains("class=\"view-empty\""));
    }

    #[test]
    fn no_external_dependencies() {
        let html = render(&doc(), &rich_messages());
        assert!(!html.contains("http://") && !html.contains("https://"), "无外链");
        assert!(!html.contains("<link"), "无外部样式表");
        assert!(!html.contains("src=\""), "无外部脚本/图片（data: URI 除外）");
        assert!(!html.contains("@import"), "无 CSS import");
        assert!(!html.contains("<iframe"), "无 iframe");
        assert!(html.contains("@media print"));
        assert!(html.contains("prefers-reduced-motion"));
        assert!(html.contains("lang=\"en\""));
        assert!(html.contains("<noscript>"));
    }

    #[test]
    fn epoch_format_is_stable() {
        assert_eq!(format_epoch(0), "Jan 1, 1970 00:00 UTC");
        assert_eq!(format_epoch(1_700_000_000), "Nov 14, 2023 22:13 UTC");
    }
}
