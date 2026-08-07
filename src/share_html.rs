//! share 页面的 HTML 渲染（`bingo share` 输出 · v4.0 Claude Code app 风格）。
//!
//! 产物是自包含单文件：CSS/JS 内嵌、零外部依赖、离线可用。展现形式参考
//! Claude Code app（用户指定方向，替代 v3.x opencode 复刻）：暗色近黑底、
//! 居中限宽消息流、用户右侧暖灰气泡、助手左侧 markdown 流、工具折叠卡
//! （状态徽标）、陶土橙 accent。事实源 = `share-page-template.html` v4.0
//! （MD5 8c29a17b）。
//!
//! 数据由 Rust 服务端渲染：所有动态文本先经 [`escape`]（`& < > " '` 全量转义）
//! 再拼进 HTML；JS 只做渐进增强（tab/锚点复制/复制按钮/线程跳转/打印），
//! 不拼接任何数据——无脚本注入面。文本块走最小 markdown→HTML（标题/粗体/
//! 行内代码/代码块/列表），不做完整 md 引擎。

use std::collections::HashMap;

use crate::api::types::{ContentBlock, ImageSource, Message, Role};
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

/// 最小 markdown → HTML（design.md v4.0 §5 安全子集）。逐行渲染，
/// 代码块为 `figure.code-block`（v4.0 模板样式依赖）。
pub fn render_markdown(text: &str) -> String {
    let mut out = String::new();
    let mut lines = text.lines().peekable();
    let mut in_code = false;
    let mut code_lang = String::new();
    let mut code_buf = String::new();
    let close_fence = |lang: &str, buf: &str, out: &mut String| {
        out.push_str("<figure class=\"code-block\">");
        if !lang.is_empty() {
            out.push_str(&format!("<figcaption>{}</figcaption>", escape(lang)));
        }
        out.push_str("<pre><code");
        if !lang.is_empty() {
            out.push_str(&format!(" class=\"language-{}\"", escape(lang)));
        }
        out.push('>');
        out.push_str(&escape(buf.trim_end()));
        out.push_str("</code></pre></figure>");
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

/// 截断到 60 字符（超长加省略号；工具卡 t-args 摘要用）。
fn clip(text: &str) -> String {
    let cut: String = text.chars().take(60).collect();
    if cut.chars().count() < text.chars().count() {
        format!("{cut}…")
    } else {
        cut
    }
}

/// 工具目标参数摘要（工具卡 t-args）：命令/文件路径/pattern 等首值。
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

/// tool_use_id → (工具名, 目标摘要)，供 ToolResult 还原工具语义与图标。
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

// ── 工具图标（15px，折叠卡 t-icon）──

const ICON_TERMINAL: &str = r#"<svg width="15" height="15" viewBox="0 0 18 18" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"><path d="M4 5l4 4-4 4M10 13h4"/></svg>"#;
const ICON_DOC: &str = r#"<svg width="15" height="15" viewBox="0 0 18 18" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"><path d="M6 2.5h6l3 3V15.5a1 1 0 0 1-1 1H6a1 1 0 0 1-1-1V3.5a1 1 0 0 1 1-1z"/><path d="M11 2.5V6h3.5"/></svg>"#;
const ICON_WRITE: &str = r#"<svg width="15" height="15" viewBox="0 0 18 18" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"><path d="M6 2.5h6l3 3V15.5a1 1 0 0 1-1 1H6a1 1 0 0 1-1-1V3.5a1 1 0 0 1 1-1z"/><path d="M11 2.5V6h3.5"/><path d="M9 8v4M7 10h4"/></svg>"#;
const ICON_EDIT: &str = r#"<svg width="15" height="15" viewBox="0 0 18 18" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"><path d="M12.5 2.5 15.5 5.5 6 15H3v-3z"/><path d="M11 4l3 3"/></svg>"#;
const ICON_GREP: &str = r#"<svg width="15" height="15" viewBox="0 0 18 18" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"><path d="M6 2.5h6l3 3V15.5a1 1 0 0 1-1 1H6a1 1 0 0 1-1-1V3.5a1 1 0 0 1 1-1z"/><circle cx="8.5" cy="10" r="1.8"/><path d="M8.5 11.8 7 13.5"/></svg>"#;
const ICON_GLOB: &str = r#"<svg width="15" height="15" viewBox="0 0 18 18" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"><circle cx="7.5" cy="7.5" r="4"/><path d="M10.5 10.5 14 14"/></svg>"#;
const ICON_LIST: &str = r#"<svg width="15" height="15" viewBox="0 0 18 18" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"><rect x="3.5" y="3.5" width="11" height="11" rx="1.5"/><path d="M6.5 7h5M6.5 10h5"/></svg>"#;
const ICON_GLOBE: &str = r#"<svg width="15" height="15" viewBox="0 0 18 18" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"><circle cx="9" cy="9" r="6"/><path d="M3 9h12M9 3a8.5 8.5 0 0 1 0 12M9 3a8.5 8.5 0 0 0 0 12"/></svg>"#;
const ICON_SPARKLE: &str = r#"<svg width="15" height="15" viewBox="0 0 18 18" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"><path d="M9 3l1.6 3.9 3.9 1.6-3.9 1.6L9 14 7.4 10.1 3.5 8.5l3.9-1.6z"/></svg>"#;

/// 工具名 → 折叠卡图标。
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

/// 成员取色（v4.0）：main 恒 accent，user 恒 text，其余按名字 FNV 哈希取 hue。
fn member_color(name: &str) -> &'static str {
    match name {
        "main" | "assistant" => "var(--accent)",
        "user" => "var(--text)",
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

/// HTML id 安全化（team/dm/channel 锚点用）。
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

/// 名字首字母（头像用）。
fn initial(name: &str) -> String {
    name.chars()
        .next()
        .map(|c| c.to_uppercase().collect::<String>())
        .unwrap_or_default()
}

/// 消息首段文本（Team 线程预览 / DM 行用）。
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

/// thinking 折叠块（灰斜体；无 token 计数——不展示该字段）。
fn think_card(thinking: &str) -> String {
    format!(
        "<details class=\"think\"><summary>Thinking</summary><div class=\"think-body\">{}</div></details>",
        render_markdown(thinking)
    )
}

/// tool_use 折叠卡：图标 + 名 + 参数摘要 + 状态徽标；input pre = 完整 JSON
/// （A4 不丢失，含 bash 非 command 字段）。
fn tool_use_card(name: &str, input: &serde_json::Value) -> String {
    let pretty = serde_json::to_string_pretty(input).unwrap_or_default();
    let target = tool_target(name, input);
    format!(
        "<details class=\"tool\" data-status=\"ok\"><summary><span class=\"t-icon\">{}</span><span class=\"t-name\">{}</span><span class=\"t-args\">{}</span><span class=\"t-status ok\">✓ done</span></summary><div class=\"t-body\"><div class=\"t-code\"><span class=\"t-label\">input</span><pre>{}</pre></div></div></details>",
        tool_icon(name),
        escape(name),
        escape(&target),
        escape(&pretty)
    )
}

/// tool_result 折叠卡：输出 pre + 状态徽标（error 红）。
fn tool_result_card(
    tool_use_id: &str,
    content: &serde_json::Value,
    is_error: bool,
    map: &HashMap<String, (String, String)>,
) -> String {
    let text = tool_result_text(content);
    let (name, target) = map
        .get(tool_use_id)
        .cloned()
        .unwrap_or_else(|| ("result".to_string(), String::new()));
    let (status_cls, status, err_cls, label) = if is_error {
        ("err", "✗ error", " err", "result · error")
    } else {
        ("ok", "✓ done", "", "result")
    };
    format!(
        "<details class=\"tool\" data-status=\"{status_cls}\"><summary><span class=\"t-icon\">{}</span><span class=\"t-name\">{}</span><span class=\"t-args\">{}</span><span class=\"t-status {status_cls}\">{status}</span></summary><div class=\"t-body\"><div class=\"t-code output{err_cls}\"><span class=\"t-label\">{label}</span><pre>{}</pre></div></div></details>",
        tool_icon(&name),
        escape(&name),
        escape(&target),
        escape(&text)
    )
}

/// 图片块：data: URI 内嵌（仅 base64，不透传外部 URL）。
fn image_html(source: &ImageSource) -> String {
    let media = escape(&source.media_type);
    let alt = format!("image ({media})");
    format!(
        "<figure style=\"max-width:var(--bubble-max)\"><img src=\"data:{media};base64,{}\" alt=\"{alt}\"></figure>",
        escape(&source.data)
    )
}

/// 单条主对话消息：user 右侧气泡 / assistant 左侧 markdown 流 + 工具折叠卡。
fn render_message(
    msg: &Message,
    index: usize,
    map: &HashMap<String, (String, String)>,
) -> String {
    let id = format!("msg-{index}");
    match msg.role {
        Role::User => {
            let mut texts = String::new();
            let mut cards = String::new();
            for block in &msg.content {
                match block {
                    ContentBlock::Text { text } => {
                        if !texts.is_empty() {
                            texts.push('\n');
                        }
                        texts.push_str(text);
                    }
                    ContentBlock::ToolResult { tool_use_id, content, is_error } => {
                        cards.push_str(&tool_result_card(tool_use_id, content, *is_error, map))
                    }
                    ContentBlock::Image { source } => cards.push_str(&image_html(source)),
                    _ => {}
                }
            }
            let bubble = if texts.trim().is_empty() {
                String::new()
            } else {
                format!("<div class=\"bubble\">{}</div>", escape(texts.trim_end()))
            };
            format!(
                "<article class=\"msg msg-user\" id=\"{id}\"><div class=\"msg-meta\"><span class=\"who\">You</span><a class=\"anchor\" href=\"#{id}\" aria-label=\"Copy link to this message\">#</a></div>{bubble}{cards}</article>"
            )
        }
        Role::Assistant => {
            let mut md = String::new();
            let mut extras = String::new();
            for block in &msg.content {
                match block {
                    ContentBlock::Text { text } => md.push_str(&render_markdown(text)),
                    ContentBlock::Thinking { thinking, .. } => extras.push_str(&think_card(thinking)),
                    ContentBlock::ToolUse { name, input, .. } => {
                        extras.push_str(&tool_use_card(name, input))
                    }
                    ContentBlock::ToolResult { tool_use_id, content, is_error } => {
                        extras.push_str(&tool_result_card(tool_use_id, content, *is_error, map))
                    }
                    ContentBlock::Image { source } => extras.push_str(&image_html(source)),
                }
            }
            let md_html = if md.trim().is_empty() {
                String::new()
            } else {
                format!("<div class=\"md\">{md}</div>")
            };
            format!(
                "<article class=\"msg msg-assistant\" id=\"{id}\"><div class=\"msg-meta\"><span class=\"who\">Assistant</span><a class=\"anchor\" href=\"#{id}\" aria-label=\"Copy link to this message\">#</a></div><div class=\"content\">{md_html}{extras}</div></article>"
            )
        }
    }
}

/// 对话视图：消息流（空态 view-empty）。
fn render_conv(messages: &[Message], map: &HashMap<String, (String, String)>) -> String {
    if messages.is_empty() {
        return "<div class=\"view-empty\">— No messages —</div>".to_string();
    }
    let mut out = String::from("<div class=\"conv\">");
    for (i, m) in messages.iter().enumerate() {
        out.push_str(&render_message(m, i + 1, map));
    }
    out.push_str("</div>");
    out
}

/// Team 视图：线程列表（点击/锚点直达私聊）。
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
        let preview = if preview.is_empty() {
            "(no history yet)".to_string()
        } else {
            escape(&preview)
        };
        rows.push_str(&format!(
            "<a class=\"thread\" id=\"team-{slug}\" href=\"#dm-{slug}\" data-jump=\"#dm-{slug}\" aria-label=\"Open {} thread\"><span class=\"t-avatar\" style=\"--c:{color}\">{}</span><div class=\"t-main\"><div class=\"t-head\"><span class=\"t-name\">{}</span><span class=\"t-state {}\">{} {}</span><span class=\"t-count\">{} msgs</span></div><div class=\"t-preview\">{preview}</div><div class=\"t-foot\">last · {} msgs · {def}</div></div></a>",
            escape(&a.name),
            initial(&a.name),
            escape(&a.name),
            escape(&a.state),
            state_glyph(&a.state),
            escape(&a.state),
            a.history.len(),
            a.history.len()
        ));
    }
    format!("<div class=\"thread-list\">{rows}</div>")
}

/// 私聊视图：每代理一个聊天流（代理左 / 用户右气泡）。
fn render_private(agents: &[AgentShare]) -> String {
    if agents.is_empty() {
        return "<div class=\"view-empty\">— No agents —</div>".to_string();
    }
    let mut out = String::from("<div class=\"dm-list\">");
    for a in agents {
        let slug = id_slug(&a.name);
        let color = member_color(&a.name);
        let def = a.def.as_deref().map(escape).unwrap_or_default();
        let mut flow = String::new();
        if a.history.is_empty() {
            flow.push_str("<p class=\"view-empty\">(no history yet)</p>");
        } else {
            for m in &a.history {
                match m.role {
                    Role::User => {
                        let text = first_text(std::slice::from_ref(m));
                        if text.is_empty() {
                            continue;
                        }
                        flow.push_str(&format!(
                            "<article class=\"msg msg-user\"><div class=\"msg-meta\"><span class=\"who\">You</span></div><div class=\"bubble\">{}</div></article>",
                            escape(&text)
                        ));
                    }
                    Role::Assistant => {
                        let mut md = String::new();
                        let mut extras = String::new();
                        for block in &m.content {
                            match block {
                                ContentBlock::Text { text } => md.push_str(&render_markdown(text)),
                                ContentBlock::Thinking { thinking, .. } => {
                                    extras.push_str(&think_card(thinking))
                                }
                                ContentBlock::ToolUse { name, input, .. } => {
                                    extras.push_str(&tool_use_card(name, input))
                                }
                                ContentBlock::Image { source } => extras.push_str(&image_html(source)),
                                ContentBlock::ToolResult { .. } => {}
                            }
                        }
                        let md_html = if md.trim().is_empty() {
                            String::new()
                        } else {
                            format!("<div class=\"md\">{md}</div>")
                        };
                        flow.push_str(&format!(
                            "<article class=\"msg msg-assistant\"><div class=\"msg-meta\"><span class=\"who\" style=\"--from:{color}\">{}</span></div>{md_html}{extras}</article>",
                            escape(&a.name)
                        ));
                    }
                }
            }
        }
        out.push_str(&format!(
            "<section class=\"dm-block\" id=\"dm-{slug}\"><header class=\"dm-head\"><span class=\"avatar\" style=\"--c:{color}\">{}</span><span class=\"name\">{}</span><span class=\"state {}\">{} {}</span><span class=\"def\">{def}</span></header><div class=\"dm-flow\">{flow}</div></section>",
            initial(&a.name),
            escape(&a.name),
            escape(&a.state),
            state_glyph(&a.state),
            escape(&a.state)
        ));
    }
    out.push_str("</div>");
    out
}

/// 频道视图：每频道消息流（seq + 发送者 + 文本，user 右对齐）。
fn render_channels(channels: &[ChannelShare]) -> String {
    if channels.is_empty() {
        return "<div class=\"view-empty\">— No channels —</div>".to_string();
    }
    let mut out = String::from("<div class=\"ch-list\">");
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
        let mut flow = String::new();
        if c.messages.is_empty() {
            flow.push_str("<p class=\"view-empty\">(no messages yet)</p>");
        } else {
            for m in &c.messages {
                let user_cls = if m.from == "user" { " ch-user" } else { "" };
                flow.push_str(&format!(
                    "<div class=\"ch-msg{user_cls}\"><span class=\"ch-seq\">#{:04}</span><span class=\"ch-from\" style=\"--from:{}\">{}</span><span class=\"ch-text\">{}</span></div>",
                    m.seq,
                    member_color(&m.from),
                    escape(&m.from),
                    escape(&m.text)
                ));
            }
        }
        out.push_str(&format!(
            "<section class=\"ch-block\" id=\"channel-{slug}\"><header class=\"ch-head\"><h3 class=\"ch-name\">◇ #{}</h3><span class=\"ch-mode {}\">{}</span><span class=\"ch-members\">{chips}</span></header><div class=\"ch-flow\">{flow}</div></section>",
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

    let mut out = String::new();
    out.push_str("<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n");
    out.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    out.push_str("<meta name=\"color-scheme\" content=\"dark\">\n");
    out.push_str("<meta name=\"generator\" content=\"bingo share\">\n");
    out.push_str(&format!("<title>bingo · {session}</title>\n"));
    out.push_str("<style>\n");
    out.push_str(CSS);
    out.push_str("</style>\n</head>\n<body>\n");
    // 顶栏：品牌 + 会话标题 + 元信息 + 四视图 tabs。
    out.push_str("<header class=\"topbar\"><div class=\"topbar-inner\"><div class=\"brand\">");
    out.push_str("<span class=\"mark\">▸</span><span class=\"name\">bingo</span>");
    out.push_str(&format!("<span class=\"session\">{session}</span>"));
    out.push_str(&format!(
        "<div class=\"meta-line\"><span>started {created}</span><span>bingo</span><button type=\"button\" class=\"print-btn\" id=\"print-btn\" aria-label=\"Print this page\">⎙ Print</button></div>"
    ));
    out.push_str("</div><nav class=\"tabs\" role=\"tablist\" aria-label=\"Views\">");
    out.push_str("<button role=\"tab\" data-tab=\"conv\" aria-selected=\"true\"><span class=\"kbd\">[1]</span>Conversation</button>");
    out.push_str(&format!(
        "<button role=\"tab\" data-tab=\"team\" aria-selected=\"false\"><span class=\"kbd\">[2]</span>Team <span class=\"count\">{}</span></button>",
        doc.agents.len()
    ));
    out.push_str(&format!(
        "<button role=\"tab\" data-tab=\"dm\" aria-selected=\"false\"><span class=\"kbd\">[3]</span>DM <span class=\"count\">{}</span></button>",
        doc.agents.len()
    ));
    out.push_str(&format!(
        "<button role=\"tab\" data-tab=\"channel\" aria-selected=\"false\"><span class=\"kbd\">[4]</span>Channels <span class=\"count\">{}</span></button>",
        doc.channels.len()
    ));
    out.push_str("</nav></div></header>\n<main id=\"main\">\n");
    // ① 对话。
    out.push_str(&format!(
        "<section class=\"view\" data-view=\"conv\" role=\"tabpanel\" id=\"view-conv\" aria-label=\"Conversation\"><h2>Conversation <span class=\"n\">· {} messages</span></h2>{}</section>\n",
        messages.len(),
        render_conv(messages, &tool_map)
    ));
    // ② Team。
    out.push_str(&format!(
        "<section class=\"view\" data-view=\"team\" role=\"tabpanel\" id=\"view-team\" aria-label=\"Team\" hidden><h2>Team <span class=\"n\">· {} threads</span></h2>{}</section>\n",
        doc.agents.len(),
        render_team(&doc.agents)
    ));
    // ③ DM。
    out.push_str(&format!(
        "<section class=\"view\" data-view=\"dm\" role=\"tabpanel\" id=\"view-dm\" aria-label=\"Direct messages\" hidden><h2>Direct Messages <span class=\"n\">· {} threads</span></h2>{}</section>\n",
        doc.agents.len(),
        render_private(&doc.agents)
    ));
    // ④ 频道。
    out.push_str(&format!(
        "<section class=\"view\" data-view=\"channel\" role=\"tabpanel\" id=\"view-channel\" aria-label=\"Channels\" hidden><h2>Channels <span class=\"n\">· {} rooms</span></h2>{}</section>\n",
        doc.channels.len(),
        render_channels(&doc.channels)
    ));
    out.push_str("</main>\n");
    // 页脚 + noscript + JS。
    out.push_str("<footer><div class=\"foot-inner\"><span><span class=\"mark\">▸</span> bingo · Rust agent CLI</span><span>generated ");
    out.push_str(&created);
    out.push_str(" by bingo share</span><span class=\"foot-warn\">contains full conversation &amp; tool output — review before sharing</span></div></footer>\n");
    out.push_str("<noscript><div style=\"padding:1.5rem;text-align:center;color:var(--faint)\">This page is fully readable without JavaScript; only tab switching, copy links and copy buttons are enhanced.</div></noscript>\n");
    out.push_str("<script>\n");
    out.push_str(JS);
    out.push_str("</script>\n</body>\n</html>\n");
    out
}

/// 内嵌样式：与 share-page-template.html v4.0（MD5 8c29a17b）原样一致
/// （Claude Code app 风格：暗色近黑底、用户右气泡、助手左流、工具折叠卡）。
const CSS: &str = include_str!("../notes/design/share-page-template.css");

/// 渐进增强 JS（与模板同源）：tab 切换、锚点复制、复制按钮、线程跳转、打印。
/// 不拼接任何会话数据（防注入）。
const JS: &str = r#"
(function(){
  'use strict';

  function activateTab(name){
    var views = document.querySelectorAll('.view[data-view]');
    for (var i = 0; i < views.length; i++){
      views[i].hidden = views[i].getAttribute('data-view') !== name;
    }
    var tabs = document.querySelectorAll('.tabs button[data-tab]');
    for (var j = 0; j < tabs.length; j++){
      var on = tabs[j].getAttribute('data-tab') === name;
      tabs[j].setAttribute('aria-selected', on ? 'true' : 'false');
      tabs[j].tabIndex = on ? 0 : -1;
    }
    if (history.replaceState) history.replaceState(null, '', '#' + name);
  }
  function bindTabs(){
    var tabs = Array.prototype.slice.call(document.querySelectorAll('.tabs button[data-tab]'));
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
      var anchor = e.target && e.target.closest ? e.target.closest('.msg-meta .anchor') : null;
      if (!anchor) return;
      e.preventDefault();
      var url = location.href.split('#')[0] + (anchor.getAttribute('href') || '');
      function fallback(){
        var ta = document.createElement('textarea');
        ta.value = url; ta.style.position = 'fixed'; ta.style.opacity = '0';
        document.body.appendChild(ta); ta.select();
        try { document.execCommand('copy'); } catch (err) {}
        document.body.removeChild(ta);
      }
      if (navigator.clipboard && navigator.clipboard.writeText){
        navigator.clipboard.writeText(url).catch(function(){ fallback(); });
      } else { fallback(); }
      anchor.textContent = '✓';
      setTimeout(function(){ anchor.textContent = '#'; }, 1600);
    });
  }

  function addCopyButtons(){
    var targets = document.querySelectorAll('.code-block, .t-code');
    for (var i = 0; i < targets.length; i++){
      (function(t){
        if (t.querySelector('.copy-btn')) return;
        var pre = t.querySelector('pre');
        var btn = document.createElement('button');
        btn.type = 'button';
        btn.className = 'copy-btn';
        btn.textContent = 'Copy';
        btn.setAttribute('aria-label', 'Copy code');
        t.appendChild(btn);
      })(targets[i]);
    }
  }

  function bindCopyButtons(){
    document.addEventListener('click', function(e){
      var btn = e.target && e.target.closest ? e.target.closest('.copy-btn') : null;
      if (!btn) return;
      var container = btn.parentElement;
      var pre = container ? container.querySelector('pre') : null;
      var text = pre ? pre.textContent : '';
      if (!text) return;
      function fallback(){
        var ta = document.createElement('textarea');
        ta.value = text; ta.style.position = 'fixed'; ta.style.opacity = '0';
        document.body.appendChild(ta); ta.select();
        try { document.execCommand('copy'); } catch (err) {}
        document.body.removeChild(ta);
      }
      if (navigator.clipboard && navigator.clipboard.writeText){
        navigator.clipboard.writeText(text).catch(function(){ fallback(); });
      } else { fallback(); }
      btn.textContent = 'Copied ✓';
      setTimeout(function(){ btn.textContent = 'Copy'; }, 1600);
    });
  }

  function bindThreadRows(){
    document.addEventListener('click', function(e){
      var row = e.target && e.target.closest ? e.target.closest('.thread[data-jump]') : null;
      if (!row) return;
      e.preventDefault();
      var target = row.getAttribute('data-jump') || '';
      var view = target.indexOf('dm') > -1 ? 'dm' : 'conv';
      activateTab(view);
      setTimeout(function(){
        var el = document.querySelector(target);
        if (el) el.scrollIntoView({ behavior: 'smooth', block: 'start' });
      }, 50);
    });
  }

  function bindPrint(){
    var printBtn = document.getElementById('print-btn');
    if (printBtn) printBtn.addEventListener('click', function(){ window.print(); });
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
      activateTab(['conv', 'team', 'dm', 'channel'].indexOf(name) >= 0 ? name : 'conv');
    });
  }

  bindTabs();
  bindAnchors();
  addCopyButtons();
  bindCopyButtons();
  bindThreadRows();
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

    /// 富消息：thinking + tool_use(bash 多字段) + tool_result + 错误结果 + text。
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
                        input: serde_json::json!({
                            "command": "ls <unsafe> & echo \"x\"",
                            "background": true,
                            "timeout": 30
                        }),
                    },
                    ContentBlock::ToolResult {
                        tool_use_id: "tu_1".into(),
                        content: serde_json::Value::String("src/ share.rs".into()),
                        is_error: false,
                    },
                    ContentBlock::ToolUse {
                        id: "tu_2".into(),
                        name: "Bash".into(),
                        input: serde_json::json!({"command": "cargo test"}),
                    },
                    ContentBlock::ToolResult {
                        tool_use_id: "tu_2".into(),
                        content: serde_json::Value::String("boom".into()),
                        is_error: true,
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
        assert!(html.contains("<figure class=\"code-block\">"), "代码块容器");
        assert!(html.contains("<figcaption>rust</figcaption>"));
        assert!(html.contains("class=\"language-rust\""));
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
        assert!(html.contains("<figure class=\"code-block\">"), "{html}");
        assert!(html.contains("<pre><code>no close</code></pre>"));
    }

    #[test]
    fn member_colors_are_stable_and_consistent() {
        assert_eq!(member_color("main"), "var(--accent)");
        assert_eq!(member_color("assistant"), "var(--accent)");
        assert_eq!(member_color("user"), "var(--text)");
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
    fn renders_all_four_views_with_cc_style() {
        let html = render(&doc(), &rich_messages());
        // 顶栏 + 四视图 data-view（v4.0 conv 也是 .view section）。
        assert!(html.contains("class=\"topbar\"") && html.contains("class=\"brand\""));
        assert!(html.contains("class=\"session\"") && html.contains("class=\"meta-line\""));
        assert!(html.contains("class=\"tabs\""));
        for view in ["data-view=\"conv\"", "data-view=\"team\"", "data-view=\"dm\"", "data-view=\"channel\""] {
            assert!(html.contains(view), "缺视图 {view}");
        }
        // 消息部件：user 气泡 / assistant markdown 流。
        assert!(html.contains("class=\"msg msg-user\""));
        assert!(html.contains("class=\"bubble\""));
        assert!(html.contains("class=\"msg msg-assistant\""));
        assert!(html.contains("class=\"md\""));
        assert!(html.contains("id=\"msg-1\""));
        assert!(html.contains("<a class=\"anchor\" href=\"#msg-1\""));
        // Team 线程列表。
        assert!(html.contains("class=\"thread-list\"") && html.contains("class=\"thread\""));
        assert!(html.contains("data-jump=\"#dm-scout\"") && html.contains("href=\"#dm-scout\""));
        assert!(html.contains("class=\"t-avatar\""));
        assert!(html.contains("2 msgs"));
        // DM 聊天流。
        assert!(html.contains("class=\"dm-block\"") && html.contains("class=\"dm-flow\""));
        assert!(html.contains("id=\"dm-scout\""));
        // 频道消息流。
        assert!(html.contains("class=\"ch-block\"") && html.contains("class=\"ch-flow\""));
        assert!(html.contains("class=\"ch-msg\""));
        assert!(html.contains("<h3 class=\"ch-name\">◇ #table</h3>"));
        assert!(html.contains("class=\"ch-mode free\""));
        assert!(html.contains("<span class=\"ch-seq\">#0001</span>"));
        assert!(html.contains("class=\"m-chip\""));
        assert!(html.contains("大家好"));
    }

    #[test]
    fn thinking_and_tool_cards_use_cc_components() {
        let html = render(&doc(), &rich_messages());
        // thinking 折叠块。
        assert!(html.contains("<details class=\"think\">"));
        assert!(html.contains("<summary>Thinking</summary>"));
        assert!(html.contains("class=\"think-body\""));
        assert!(html.contains("深入思考一下"));
        // 工具折叠卡：t-icon + t-name + t-args + 状态徽标。
        assert!(html.contains("<details class=\"tool\" data-status=\"ok\">"));
        assert!(html.contains("<details class=\"tool\" data-status=\"err\">"));
        assert!(html.contains("<span class=\"t-icon\">"));
        assert!(html.contains("<span class=\"t-name\">Bash</span>"));
        assert!(html.contains("<span class=\"t-args\">cargo test</span>"));
        assert!(html.contains("<span class=\"t-status ok\">✓ done</span>"));
        assert!(html.contains("<span class=\"t-status err\">✗ error</span>"));
        assert!(html.contains("class=\"t-body\""));
        assert!(html.contains("<span class=\"t-label\">input</span>"));
        assert!(html.contains("<span class=\"t-label\">result</span>"));
        assert!(html.contains("result · error"));
        // 折叠默认收起（无 open 属性）。
        assert!(!html.contains("<details class=\"tool\" data-status=\"ok\" open"));
    }

    #[test]
    fn bash_input_full_json_preserved() {
        // A4（v4.0 契约）：bash input pre 始终含完整 JSON（含非 command 字段）。
        let html = render(&doc(), &rich_messages());
        assert!(html.contains("&quot;background&quot;: true"), "background 保留");
        assert!(html.contains("&quot;timeout&quot;: 30"), "timeout 保留");
        assert!(html.contains("&quot;command&quot;"), "command 键保留");
        assert!(html.contains("ls &lt;unsafe&gt; &amp; echo"), "command 值实体化呈现");
        assert!(!html.contains("<img src=x onerror"), "无未转义标签");
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
        assert!(html.contains("addCopyButtons"), "复制按钮创建逻辑存在");
    }

    #[test]
    fn epoch_format_is_stable() {
        assert_eq!(format_epoch(0), "Jan 1, 1970 00:00 UTC");
        assert_eq!(format_epoch(1_700_000_000), "Nov 14, 2023 22:13 UTC");
    }
}
