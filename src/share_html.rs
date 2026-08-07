//! share 页面的 HTML 渲染（`bingo share` 输出）。
//!
//! 产物是自包含单文件：CSS/JS 内嵌、零外部依赖、离线可用。结构与样式
//! 对齐 `notes/design/share-page-template.html` v2.0（ui-ux 唯一事实源：
//! 浅色文档底、消息左装饰列 + 限宽内容、工具两段式、锚点复制链接）。
//! 所有动态文本在 Rust 侧先经 [`escape`]（`& < > " '` 全量转义）再拼进
//! HTML；JS 只做渐进增强（tab/复制/打印），不拼接任何数据——无注入面。
//! 文本块走最小 markdown→HTML（标题/粗体/行内代码/代码块/列表），不做
//! 完整 md 引擎。

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

/// 最小 markdown → HTML。逐行渲染（换行即保留），代码块原样并带
/// `.code-block` 容器（模板样式依赖）。
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

/// 工具目标参数摘要（标题行 t-args）：命令/文件路径/pattern 等首值，≤60 字符。
/// 完整 input 仍在结果块 `<pre>` 原样呈现（A4 不截断）。
fn tool_args(input: &serde_json::Value) -> String {
    const KEYS: [&str; 8] = [
        "command", "file_path", "pattern", "query", "subject", "prompt", "path", "skill",
    ];
    let picked = KEYS
        .iter()
        .find_map(|k| input.get(*k).and_then(|v| v.as_str()))
        .map(str::to_string)
        .or_else(|| {
            if let serde_json::Value::Object(map) = input {
                map.values().find_map(|v| v.as_str()).map(str::to_string)
            } else {
                None
            }
        })
        .unwrap_or_default();
    let cut: String = picked.chars().take(60).collect();
    if cut.chars().count() < picked.chars().count() {
        format!("{cut}…")
    } else {
        cut
    }
}

/// 成员取色：main/assistant 恒 accent，user 恒 text，其余按名字 FNV 哈希取
/// hue-0..5（与模板 .m-chip/.dm-row/.ch-row 的 --chip/--from 令牌对应）。
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

/// 消息锚点 SVG（链环图标；hover 变色，点击复制 URL#id——JS 增强）。
const ANCHOR_SVG: &str = r#"<svg viewBox="0 0 16 16"><path d="M6.05 9.95a3 3 0 0 0 4.24 0l2.83-2.83a3 3 0 0 0-4.24-4.24L7.5 4.26" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/><path d="M9.95 6.05a3 3 0 0 0-4.24 0l-2.83 2.83a3 3 0 0 0 4.24 4.24l1.38-1.38" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/></svg>"#;

fn anchor(id: &str) -> String {
    format!(
        "<a class=\"anchor\" href=\"#{id}\" aria-label=\"Copy link to this message\">{ANCHOR_SVG}</a><span class=\"line\"></span>"
    )
}

/// 非文本块（thinking / tool_use / tool_result / image）→ 折叠卡或工具两段式。
fn render_block_extra(block: &ContentBlock) -> String {
    match block {
        ContentBlock::Thinking { thinking, .. } => format!(
            "<details class=\"think\"><summary>Thinking</summary><div class=\"think-body\">{}</div></details>",
            render_markdown(thinking)
        ),
        ContentBlock::ToolUse { name, input, .. } => {
            // input 经 serde_json 重序列化（键序可能与原文不同，内容语义等价；
            // A4「原样呈现」按内容等价验收）。
            let pretty = serde_json::to_string_pretty(input).unwrap_or_default();
            format!(
                "<div class=\"tool\"><div class=\"tool-title\"><span class=\"t-ico\">⏺</span><span class=\"t-name\">{}</span><span class=\"t-args\">{}</span></div><details class=\"tool-result w-sm\"><summary>Show result</summary><div class=\"t-body\"><div class=\"t-code\"><span class=\"t-label\">input</span><pre>{}</pre></div></div></details></div>",
                escape(name),
                escape(&tool_args(input)),
                escape(&pretty)
            )
        }
        ContentBlock::ToolResult { content, is_error, .. } => {
            let (status_cls, status, label, err_cls) = if *is_error {
                ("err", "✗", "result (err)", " err")
            } else {
                ("ok", "✓", "result", "")
            };
            format!(
                "<div class=\"tool\"><div class=\"tool-title\"><span class=\"t-ico\">⏺</span><span class=\"t-name\">tool_result</span><span class=\"t-status {status_cls}\">{status}</span></div><details class=\"tool-result w-sm\"><summary>Show result</summary><div class=\"t-body\"><div class=\"t-code output{err_cls}\"><span class=\"t-label\">{label}</span><pre>{}</pre></div></div></details></div>",
                escape(&tool_result_text(content))
            )
        }
        ContentBlock::Image { source } => {
            // 图片仅允许 data: URI（transcript 的 base64 块直接内嵌，离线可见；
            // 不透传任何外部 URL，PRD C3）。
            let media = escape(&source.media_type);
            let alt = format!("image ({media})");
            format!(
                "<figure class=\"img-block\"><img src=\"data:{media};base64,{}\" alt=\"{alt}\"></figure>",
                escape(&source.data)
            )
        }
        ContentBlock::Text { .. } => String::new(),
    }
}

/// 单条主对话消息：左装饰列（锚点 + 贯穿竖线）+ 限宽内容列。
/// user 无框纯文本；assistant 文本进 `.card` 细框卡，thinking/tool 为兄弟块。
fn render_message(msg: &Message, index: usize) -> String {
    let id = format!("msg-{index}");
    let (role_cls, meta_cls, label) = match msg.role {
        Role::User => ("msg-user", "role-user", "User"),
        Role::Assistant => ("msg-assistant", "role-assistant", "Assistant"),
    };
    let mut texts = String::new();
    let mut extra = String::new();
    for block in &msg.content {
        match block {
            ContentBlock::Text { text } => texts.push_str(&render_markdown(text)),
            other => extra.push_str(&render_block_extra(other)),
        }
    }
    let content = if msg.role == Role::Assistant {
        let card = if texts.trim().is_empty() {
            String::new()
        } else {
            format!("<div class=\"card\">{texts}</div>")
        };
        format!("{card}{extra}")
    } else {
        format!("<div class=\"body\">{texts}{extra}</div>")
    };
    format!(
        "<article class=\"msg {role_cls}\" id=\"{id}\"><div class=\"dec\" aria-hidden=\"true\">{}</div><div class=\"content w-md\"><div class=\"meta\"><span class=\"{meta_cls}\">{label}</span></div>{content}</div></article>",
        anchor(&id)
    )
}

fn render_messages(messages: &[Message]) -> String {
    if messages.is_empty() {
        return "<div class=\"empty\">— No messages —</div>".to_string();
    }
    let mut out = String::from("<div class=\"conv\">");
    for (i, m) in messages.iter().enumerate() {
        out.push_str(&render_message(m, i + 1));
    }
    out.push_str("</div>");
    out
}

fn render_team(agents: &[AgentShare]) -> String {
    if agents.is_empty() {
        return "<div class=\"empty\">— No agents —</div>".to_string();
    }
    let mut out = String::from("<div class=\"roster\">");
    for a in agents {
        let def = a.def.as_deref().map(escape).unwrap_or_else(|| "—".to_string());
        out.push_str(&format!(
            "<div class=\"r-row\"><span class=\"r-state {}\">● {}</span><span class=\"r-name\">{}</span><span class=\"r-meta\">{} · {} messages</span><span class=\"r-desc\">{}</span></div>",
            escape(&a.state),
            escape(&a.state),
            escape(&a.name),
            def,
            a.history.len(),
            escape(&a.description)
        ));
    }
    out.push_str("</div>");
    out
}

/// 私聊视图：每个子代理一段完整历史（SendMessage 续话即该实例的 history）。
fn render_private(agents: &[AgentShare]) -> String {
    if agents.is_empty() {
        return "<div class=\"empty\">— No agents —</div>".to_string();
    }
    let mut out = String::from("<div class=\"dm-list\">");
    for a in agents {
        let mut thread = String::new();
        if a.history.is_empty() {
            thread.push_str("<div class=\"dm-thread\"><p class=\"empty\">(no history yet)</p></div>");
        } else {
            thread.push_str("<div class=\"dm-thread\">");
            for m in &a.history {
                let (from, user_cls, from_color) = match m.role {
                    Role::User => ("user", " dm-user", "var(--text)"),
                    Role::Assistant => (a.name.as_str(), "", ""),
                };
                let from_color = if from_color.is_empty() {
                    member_color(&a.name)
                } else {
                    from_color
                };
                let mut texts = String::new();
                let mut extra = String::new();
                for block in &m.content {
                    match block {
                        ContentBlock::Text { text } => texts.push_str(&render_markdown(text)),
                        other => extra.push_str(&render_block_extra(other)),
                    }
                }
                thread.push_str(&format!(
                    "<div class=\"dm-row{user_cls}\"><span class=\"dm-from\" style=\"--from:{from_color}\">{}</span><span class=\"dm-text\">{}{}</span><span class=\"dm-time\"></span></div>",
                    escape(from),
                    texts,
                    extra
                ));
            }
            thread.push_str("</div>");
        }
        out.push_str(&format!(
            "<details class=\"dm-agent\"><summary><span class=\"a-dot\" style=\"--dot:{}\"></span><span class=\"a-name\">{}</span><span class=\"a-state\">{}</span><span class=\"a-count\">{} messages</span></summary>{}</details>",
            member_color(&a.name),
            escape(&a.name),
            escape(&a.state),
            a.history.len(),
            thread
        ));
    }
    out.push_str("</div>");
    out
}

fn render_channels(channels: &[ChannelShare]) -> String {
    if channels.is_empty() {
        return "<div class=\"empty\">— No channels —</div>".to_string();
    }
    let mut out = String::from("<div class=\"ch-list\">");
    for c in channels {
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
            stream.push_str("<p class=\"empty\">(no messages yet)</p>");
        } else {
            stream.push_str("<div class=\"ch-stream\">");
            for m in &c.messages {
                let user_cls = if m.from == "user" { " ch-user" } else { "" };
                stream.push_str(&format!(
                    "<div class=\"ch-row{user_cls}\"><span class=\"ch-seq\">{}</span><span class=\"ch-from\" style=\"--from:{}\">{}</span><span class=\"ch-text\">{}</span></div>",
                    m.seq,
                    member_color(&m.from),
                    escape(&m.from),
                    render_markdown(&m.text)
                ));
            }
            stream.push_str("</div>");
        }
        out.push_str(&format!(
            "<div class=\"ch-block\"><div class=\"ch-head\"><span class=\"ch-name\">◇ #{}</span><span class=\"ch-mode {}\">{}</span><span class=\"ch-members\">{}</span></div>{}</div>",
            escape(&c.name),
            escape(&c.mode),
            escape(&c.mode),
            chips,
            stream
        ));
    }
    out.push_str("</div>");
    out
}

/// 生成自包含 HTML 文档（conversation 来自主 transcript，其余来自 ShareDoc）。
pub fn render(doc: &ShareDoc, messages: &[Message]) -> String {
    let session = escape(&doc.session);
    let mut out = String::new();
    out.push_str("<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n");
    out.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    out.push_str("<meta name=\"color-scheme\" content=\"light\">\n");
    out.push_str("<meta name=\"generator\" content=\"bingo share\">\n");
    out.push_str(&format!("<title>bingo · {session}</title>\n"));
    out.push_str("<style>\n");
    out.push_str(CSS);
    out.push_str("</style>\n</head>\n<body>\n");
    out.push_str("<a class=\"skip\" href=\"#main\">Skip to content</a>\n");
    out.push_str("<header class=\"topbar\"><div class=\"topbar-inner\"><div class=\"brand\">");
    out.push_str("<span class=\"mark\">▸</span><span class=\"name\">bingo</span>");
    out.push_str(&format!("<span class=\"session\">{session}</span>"));
    out.push_str(&format!(
        "<div class=\"meta-line\"><span id=\"meta-time\" data-ts=\"{}\"></span>",
        doc.created_at
    ));
    out.push_str("<button type=\"button\" class=\"print-btn\" id=\"print-btn\" aria-label=\"Print this page\">⎙ Print</button></div>");
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
    out.push_str(&format!(
        "<section class=\"view\" data-view=\"conv\" role=\"tabpanel\" id=\"view-conv\" aria-label=\"Conversation\"><h2>Conversation <span class=\"n\">· {} messages</span></h2>{}</section>\n",
        messages.len(),
        render_messages(messages)
    ));
    out.push_str(&format!(
        "<section class=\"view\" data-view=\"team\" role=\"tabpanel\" id=\"view-team\" aria-label=\"Team roster\" hidden><h2>Team <span class=\"n\">· {} agents</span></h2>{}</section>\n",
        doc.agents.len(),
        render_team(&doc.agents)
    ));
    out.push_str(&format!(
        "<section class=\"view\" data-view=\"dm\" role=\"tabpanel\" id=\"view-dm\" aria-label=\"Direct messages\" hidden><h2>DM <span class=\"n\">· one thread per agent</span></h2>{}</section>\n",
        render_private(&doc.agents)
    ));
    out.push_str(&format!(
        "<section class=\"view\" data-view=\"channel\" role=\"tabpanel\" id=\"view-channel\" aria-label=\"Channels\" hidden><h2>Channels <span class=\"n\">· {} rooms</span></h2>{}</section>\n",
        doc.channels.len(),
        render_channels(&doc.channels)
    ));
    out.push_str("</main>\n<footer><div class=\"foot-inner\"><span><span class=\"mark\">▸</span> bingo · Rust agent CLI</span><span>generated <span id=\"foot-gen\" data-ts=\"\"></span> by bingo share</span><span class=\"foot-warn\">contains full conversation &amp; tool output — review before sharing</span></div></footer>\n");
    out.push_str("<noscript><div style=\"padding:24px;text-align:center;color:var(--faint)\">This page works without JavaScript; only tab switching, copy links and print expansion are enhanced.</div></noscript>\n");
    out.push_str("<script>\n");
    out.push_str(JS);
    out.push_str("</script>\n</body>\n</html>\n");
    out
}

/// 内嵌样式：与 notes/design/share-page-template.html v2.0 同源（ui-ux 唯一事实源）。
const CSS: &str = r#"
:root{
  --bg:#FAFAF7; --bg-card:#FFFFFF; --bg-subtle:#F3F2EF;
  --hairline:#E5E4DF; --hairline-strong:#D8D7D1;
  --text:#1F2328; --secondary:#57606A; --dimmed:#646D76; --faint:#8A929B;
  --accent:#B05227; --accent-soft:#D77757; --accent-border:#E7C4B2;
  --green:#1A7F37; --red:#CF222E; --gold:#9A6700; --info:#146C7A;
  --hue-0:#0F766E; --hue-1:#4A58C8; --hue-2:#6E40C9;
  --hue-3:#8A6418; --hue-4:#B03A8B; --hue-5:#2F7D32;
  --font-mono:ui-monospace,"SF Mono","JetBrains Mono","Cascadia Code",Menlo,Consolas,"Liberation Mono","DejaVu Sans Mono",monospace;
  --font-sans:-apple-system,"SF Pro Text","Segoe UI","Noto Sans SC","PingFang SC","Microsoft YaHei",sans-serif;
  --fs:0.875rem; --fs-sm:0.8125rem; --fs-xs:0.75rem; --lh:1.6;
  --s1:4px; --s2:8px; --s3:12px; --s4:16px; --s5:24px; --s6:32px; --s7:48px;
  --radius:4px;
  --maxw:880px;
  --w-sm:480px; --w-md:640px; --w-lg:760px;
}
*,*::before,*::after{box-sizing:border-box}
html{-webkit-text-size-adjust:100%}
body{margin:0;background:var(--bg);color:var(--text);
  font-family:var(--font-sans);font-size:var(--fs);line-height:var(--lh);
  -webkit-font-smoothing:antialiased}
[hidden]{display:none!important}
::selection{background:var(--accent-soft);color:#fff}
a{color:var(--accent)}
a:hover{text-decoration-thickness:2px}
:focus-visible{outline:2px solid var(--accent);outline-offset:2px;border-radius:2px}
code,kbd,pre,.mono{font-family:var(--font-mono)}
.skip{position:absolute;left:var(--s4);top:var(--s4);z-index:50;padding:var(--s2) var(--s3);
  background:var(--accent);color:#fff;text-decoration:none;border-radius:var(--radius);
  transform:translateY(-300%);transition:transform .12s}
.skip:focus{transform:none}
.topbar{position:sticky;top:0;z-index:10;background:var(--bg);border-bottom:1px solid var(--hairline)}
.topbar-inner{max-width:var(--maxw);margin:0 auto;padding:0 var(--s4)}
.brand{display:flex;align-items:baseline;gap:var(--s2);padding:var(--s3) 0 var(--s2)}
.brand .mark{color:var(--accent);font-weight:700}
.brand .name{font-weight:700;letter-spacing:.02em;font-family:var(--font-mono)}
.brand .session{color:var(--secondary);font-size:var(--fs-sm);overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.brand .session::before{content:"· ";color:var(--faint)}
.meta-line{display:flex;align-items:center;gap:var(--s3);margin-left:auto;color:var(--faint);
  font-size:var(--fs-xs);white-space:nowrap;overflow:hidden}
.print-btn{appearance:none;border:1px solid var(--hairline-strong);background:var(--bg-card);color:var(--secondary);
  font:inherit;font-size:var(--fs-xs);padding:2px 8px;border-radius:var(--radius);cursor:pointer}
.print-btn:hover{color:var(--accent);border-color:var(--accent)}
.tabs{display:flex;gap:0;border-top:1px solid var(--hairline)}
.tabs button{appearance:none;background:transparent;border:0;border-bottom:2px solid transparent;
  color:var(--dimmed);font:inherit;font-size:var(--fs-sm);padding:var(--s2) var(--s4) calc(var(--s2) + 1px);
  cursor:pointer;position:relative;top:-1px}
.tabs button:hover{color:var(--text)}
.tabs button[aria-selected="true"]{color:var(--text);border-bottom-color:var(--accent)}
.tabs .count{color:var(--faint);font-size:var(--fs-xs);margin-left:2px}
.tabs button[aria-selected="true"] .count{color:var(--accent)}
.tabs .kbd{color:var(--faint);margin-right:var(--s1)}
main{max-width:var(--maxw);margin:0 auto;padding:var(--s6) var(--s4) var(--s7)}
.view[data-view]{animation:view-in .12s ease-out}
@keyframes view-in{from{opacity:0;transform:translateY(4px)}to{opacity:1;transform:none}}
.view h2{display:flex;align-items:baseline;gap:var(--s2);margin:0 0 var(--s4);
  font-size:var(--fs);font-weight:700;color:var(--text);font-family:var(--font-mono)}
.view h2 .n{color:var(--faint);font-weight:400}
.empty{color:var(--faint);padding:var(--s7) 0;text-align:center}
.conv{display:flex;flex-direction:column}
.msg{display:grid;grid-template-columns:22px minmax(0,1fr);column-gap:14px;margin-bottom:1.25rem}
.dec{display:flex;flex-direction:column;align-items:center}
.dec .anchor{width:18px;height:18px;flex:0 0 auto;display:flex;align-items:center;justify-content:center;
  color:var(--faint);border-radius:var(--radius);text-decoration:none}
.dec .anchor:hover{color:var(--accent);background:var(--bg-subtle)}
.dec .anchor svg{width:12px;height:12px;fill:currentColor}
.dec .anchor::after{content:"";position:absolute;transform:translateY(20px);white-space:nowrap;
  font-size:var(--fs-xs);color:#fff;background:var(--text);padding:2px 8px;border-radius:var(--radius);
  opacity:0;pointer-events:none;transition:opacity .12s;z-index:5}
.dec .anchor.copied::after{content:"Copied ✓";opacity:1}
.dec .line{width:3px;flex:1;border-radius:1px;background:var(--hairline-strong);margin-top:6px}
.content{min-width:0}
.content .w-sm{max-width:var(--w-sm)}
.content.w-sm{max-width:var(--w-sm)}
.content.w-md{max-width:var(--w-md)}
.content.w-lg{max-width:var(--w-lg)}
.meta{font-size:var(--fs-xs);color:var(--secondary);text-transform:uppercase;
  letter-spacing:.05em;margin-bottom:var(--s2);display:flex;align-items:baseline;gap:var(--s2)}
.meta time{color:var(--faint);letter-spacing:0;text-transform:none}
.meta .role-user{color:var(--secondary)}
.meta .role-assistant{color:var(--accent)}
.body p{margin:0 0 var(--s2)}
.body p:last-child{margin-bottom:0}
.card{border:1px solid var(--accent-border);background:var(--bg-card);
  border-radius:var(--radius);padding:0.5rem;margin-bottom:var(--s3)}
.card>:first-child{margin-top:0}
.card>:last-child{margin-bottom:0}
.card h1,.card h2,.card h3,.card h4{margin:var(--s4) 0 var(--s2);line-height:1.4;font-family:var(--font-mono)}
.card h1{font-size:1.2em;border-bottom:1px solid var(--hairline);padding-bottom:var(--s1)}
.card h2{font-size:1.1em}.card h3{font-size:1em}
.card p{margin:var(--s2) 0}
.card ul,.card ol{margin:var(--s2) 0;padding-left:var(--s5)}
.card li{margin:2px 0}
.card code{background:var(--bg-subtle);border:1px solid var(--hairline);border-radius:3px;
  padding:0 4px;font-size:.92em;color:var(--info)}
.card pre code{background:none;border:0;padding:0;color:inherit;font-size:inherit}
.card blockquote{margin:var(--s3) 0;padding-left:var(--s4);border-left:3px solid var(--hairline-strong);color:var(--dimmed)}
.card hr{border:0;border-top:1px solid var(--hairline);margin:var(--s4) 0}
.card table{border-collapse:collapse;margin:var(--s3) 0;font-size:var(--fs-sm);width:100%}
.card th,.card td{border:1px solid var(--hairline);padding:4px var(--s3);text-align:left}
.card th{background:var(--bg-subtle);font-weight:600}
.card del{color:var(--faint)}
.code-block{margin:var(--s3) 0;background:var(--bg-subtle);border:1px solid var(--hairline);
  border-radius:var(--radius);position:relative}
.code-block figcaption{position:absolute;top:6px;right:34px;color:var(--faint);font-size:var(--fs-xs)}
.code-block pre{margin:0;padding:var(--s3) var(--s4);overflow-x:auto;
  font-family:var(--font-mono);font-size:var(--fs-sm);line-height:1.6;color:var(--text)}
.copy-btn{position:absolute;top:4px;right:6px;appearance:none;border:1px solid var(--hairline);
  background:var(--bg-card);color:var(--secondary);font:inherit;font-size:var(--fs-xs);padding:2px 8px;
  border-radius:3px;cursor:pointer;opacity:0;transition:opacity .12s}
.code-block:hover .copy-btn,.copy-btn:focus-visible,.t-code:hover .copy-btn{opacity:1}
.copy-btn:hover{color:var(--accent);border-color:var(--accent)}
.img-block{margin:var(--s3) 0;border:1px solid var(--hairline);border-radius:var(--radius);
  background:var(--bg-card);padding:var(--s2);text-align:center}
.img-block img{max-width:100%;height:auto;display:block;margin:0 auto}
details.think{border:1px solid var(--accent-border);background:var(--bg-card);
  border-radius:var(--radius);padding:0.5rem;margin-bottom:var(--s3)}
details.think summary{cursor:pointer;color:var(--secondary);font-size:var(--fs-xs);
  text-transform:uppercase;letter-spacing:.05em;list-style:none;display:flex;align-items:center;gap:var(--s2)}
details.think summary::-webkit-details-marker{display:none}
details.think summary::before{content:"∴";color:var(--accent-soft);font-size:1em;text-transform:none}
details.think summary:hover{color:var(--text)}
details.think[open] summary{margin-bottom:var(--s2)}
.think-body{color:var(--dimmed);font-size:var(--fs-sm);white-space:pre-wrap;line-height:1.7}
.tool{margin-bottom:var(--s3)}
.tool-title{display:flex;align-items:baseline;gap:var(--s2);font-size:var(--fs-sm);
  font-family:var(--font-mono);color:var(--secondary);line-height:18px}
.tool-title .t-ico{color:var(--info);font-size:.9em}
.tool-title .t-name{font-weight:700}
.tool-title .t-args{color:var(--faint);overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.tool-title .t-status{margin-left:auto;font-weight:700;white-space:nowrap}
.tool-title .t-status.ok{color:var(--green)}
.tool-title .t-status.err{color:var(--red)}
details.tool-result{margin-top:var(--s1)}
details.tool-result summary{cursor:pointer;color:var(--secondary);font-size:var(--fs-xs);
  text-transform:uppercase;letter-spacing:.05em;list-style:none;display:inline-flex;align-items:center;gap:var(--s1)}
details.tool-result summary::-webkit-details-marker{display:none}
details.tool-result summary::before{content:"▸";color:var(--accent);transition:transform .12s}
details.tool-result[open] summary::before{transform:rotate(90deg)}
details.tool-result summary:hover{color:var(--accent)}
details.tool-result .t-body{margin-top:var(--s2);display:grid;gap:var(--s2)}
.t-code{background:var(--bg-subtle);border:1px solid var(--hairline);border-radius:var(--radius)}
.t-code .t-label{display:block;padding:4px var(--s3);color:var(--faint);font-size:var(--fs-xs);
  border-bottom:1px solid var(--hairline);text-transform:uppercase;letter-spacing:.05em}
.t-code pre{margin:0;padding:var(--s2) var(--s3);overflow-x:auto;
  font-family:var(--font-mono);font-size:var(--fs-sm);line-height:1.6;color:var(--text);
  white-space:pre-wrap;word-break:break-word}
.t-code.output pre{color:var(--dimmed)}
.t-code.output.err{border-color:#E5B4B8}
.t-code.output.err .t-label{color:var(--red)}
.roster{border:1px solid var(--hairline);border-radius:var(--radius);background:var(--bg-card)}
.r-row{display:grid;grid-template-columns:92px 120px 1fr;gap:var(--s2) var(--s4);
  padding:var(--s3) var(--s4);align-items:baseline}
.r-row+.r-row{border-top:1px solid var(--hairline)}
.r-row:hover{background:var(--bg-subtle)}
.r-state{font-size:var(--fs-sm);white-space:nowrap;color:var(--faint)}
.r-state.idle{color:var(--faint)}
.r-state.running{color:var(--info)}
.r-state.stopped{color:var(--red)}
.r-name{font-family:var(--font-mono);font-weight:700;color:var(--accent);
  overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.r-meta{color:var(--secondary);font-size:var(--fs-xs);white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
.r-desc{color:var(--dimmed);font-size:var(--fs-sm);grid-column:2 / -1;margin-top:-4px}
.dm-list{display:grid;gap:var(--s3)}
.dm-agent{border:1px solid var(--hairline);border-radius:var(--radius);background:var(--bg-card)}
.dm-agent summary{cursor:pointer;padding:var(--s2) var(--s4);list-style:none;display:flex;align-items:center;gap:var(--s3)}
.dm-agent summary::-webkit-details-marker{display:none}
.dm-agent summary .a-dot{width:8px;height:8px;border-radius:50%;background:var(--dot,var(--faint))}
.dm-agent summary .a-name{font-family:var(--font-mono);font-weight:700}
.dm-agent summary .a-state{color:var(--faint);font-size:var(--fs-xs)}
.dm-agent summary .a-count{margin-left:auto;color:var(--faint);font-size:var(--fs-xs)}
.dm-agent summary:hover{background:var(--bg-subtle)}
.dm-agent[open] summary{border-bottom:1px solid var(--hairline)}
.dm-thread{padding:var(--s2) 0}
.dm-row{display:grid;grid-template-columns:80px 1fr auto;gap:var(--s3);padding:var(--s1) var(--s4);align-items:baseline}
.dm-row .dm-from{font-family:var(--font-mono);font-weight:700;color:var(--from,var(--accent));
  overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.dm-row .dm-text{color:var(--text);white-space:pre-wrap;word-break:break-word;font-size:var(--fs-sm)}
.dm-row .dm-time{color:var(--faint);font-size:var(--fs-xs);white-space:nowrap}
.dm-row.dm-user .dm-text{text-align:right;color:var(--dimmed)}
.dm-row.dm-user{grid-template-columns:1fr 80px auto}
.ch-list{display:grid;gap:var(--s4)}
.ch-block{border:1px solid var(--hairline);border-radius:var(--radius);background:var(--bg-card)}
.ch-head{display:flex;align-items:center;gap:var(--s3);flex-wrap:wrap;padding:var(--s3) var(--s4);border-bottom:1px solid var(--hairline)}
.ch-head .ch-name{margin:0;font-size:var(--fs);font-weight:700;font-family:var(--font-mono);color:var(--accent)}
.ch-mode{font-size:var(--fs-xs);padding:1px 8px;border:1px solid;border-radius:999px;text-transform:uppercase;letter-spacing:.04em}
.ch-mode.serial{color:var(--info);border-color:rgba(20,108,122,.4);background:rgba(20,108,122,.06)}
.ch-mode.free{color:var(--hue-1);border-color:rgba(74,88,200,.4);background:rgba(74,88,200,.06)}
.ch-members{margin-left:auto;display:flex;gap:var(--s1);flex-wrap:wrap}
.m-chip{font-size:var(--fs-xs);color:var(--dimmed);display:inline-flex;align-items:center;gap:4px}
.m-chip::before{content:"";width:6px;height:6px;border-radius:50%;background:var(--chip,var(--faint))}
.ch-stream{padding:var(--s2) 0}
.ch-row{display:grid;grid-template-columns:48px 90px 1fr;gap:var(--s3);padding:var(--s1) var(--s4);align-items:baseline}
.ch-row .ch-seq{color:var(--faint);font-size:var(--fs-xs);text-align:right;font-family:var(--font-mono)}
.ch-row .ch-from{font-family:var(--font-mono);font-weight:700;color:var(--from,var(--accent));
  overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.ch-row .ch-text{color:var(--text);white-space:pre-wrap;word-break:break-word}
.ch-row.ch-user{grid-template-columns:1fr 90px 48px}
.ch-row.ch-user .ch-text{order:1;text-align:right;color:var(--dimmed)}
.ch-row.ch-user .ch-from{order:2}
.ch-row.ch-user .ch-seq{order:3}
.ch-row+.ch-row{border-top:1px dashed var(--hairline)}
footer{border-top:1px solid var(--hairline);color:var(--faint);font-size:var(--fs-xs)}
.foot-inner{max-width:var(--maxw);margin:0 auto;padding:var(--s3) var(--s4);display:flex;gap:var(--s3);flex-wrap:wrap}
.foot-inner .mark{color:var(--accent)}
.foot-warn{color:var(--gold)}
@media (max-width:640px){
  .meta-line{display:none}
  .r-row{grid-template-columns:80px 1fr;grid-template-areas:"state name" "state meta" "desc desc"}
  .r-state{grid-area:state}.r-name{grid-area:name}.r-meta{grid-area:meta}.r-desc{grid-area:desc;margin-top:0}
  .dm-row{grid-template-columns:64px 1fr}
  .dm-row .dm-time{display:none}
  .dm-row.dm-user{grid-template-columns:1fr 64px}
  .ch-row{grid-template-columns:40px 68px 1fr}
  .ch-row.ch-user{grid-template-columns:1fr 68px 40px}
  .brand .session{max-width:40vw}
  .content.w-sm,.content.w-md,.content.w-lg{max-width:100%}
}
@media print{
  :root{--bg:#FFFFFF;--bg-subtle:#F5F5F3}
  .topbar{position:static}
  .tabs,.copy-btn,.print-btn,.skip,.dec .anchor{display:none!important}
  .dec .line{background:var(--hairline)}
  body{font-size:12px}
  .msg{break-inside:avoid}
  pre,details summary,figcaption{break-inside:avoid}
  a{text-decoration:underline}
}
@media (prefers-reduced-motion:reduce){
  *{transition:none!important;animation:none!important}
}
"#;

/// 渐进增强 JS（与模板同源）：tab 切换、消息锚点复制、代码复制、打印展开。
/// 不拼接任何会话数据（防注入）。
const JS: &str = r#"
(function(){
  'use strict';

  function activateTab(name){
    var panels = document.querySelectorAll('.view');
    for (var i = 0; i < panels.length; i++){
      panels[i].hidden = panels[i].getAttribute('data-view') !== name;
    }
    var tabs = document.querySelectorAll('.tabs button');
    for (var j = 0; j < tabs.length; j++){
      var on = tabs[j].getAttribute('data-tab') === name;
      tabs[j].setAttribute('aria-selected', on ? 'true' : 'false');
      tabs[j].tabIndex = on ? 0 : -1;
    }
    if (history.replaceState) history.replaceState(null, '', '#' + name);
  }
  function bindTabs(){
    var tabs = Array.prototype.slice.call(document.querySelectorAll('.tabs button'));
    tabs.forEach(function(btn){
      btn.addEventListener('click', function(){ activateTab(btn.getAttribute('data-tab')); });
      btn.addEventListener('keydown', function(e){
        var idx = tabs.indexOf(btn);
        var next;
        if (e.key === 'ArrowRight' || e.key === 'ArrowDown'){
          e.preventDefault();
          next = tabs[(idx + 1) % tabs.length];
        } else if (e.key === 'ArrowLeft' || e.key === 'ArrowUp'){
          e.preventDefault();
          next = tabs[(idx - 1 + tabs.length) % tabs.length];
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
    var anchors = document.querySelectorAll('.dec .anchor');
    for (var i = 0; i < anchors.length; i++){
      anchors[i].addEventListener('click', function(e){
        e.preventDefault();
        var target = this.getAttribute('href');
        var url = location.href.split('#')[0] + target;
        var done = function(ok){
          this.classList.add('copied');
          setTimeout(function(){ this.classList.remove('copied'); }.bind(this), 1600);
        }.bind(this);
        var fallback = function(){
          var ta = document.createElement('textarea');
          ta.value = url;
          ta.style.position = 'fixed';
          ta.style.opacity = '0';
          document.body.appendChild(ta);
          ta.select();
          var ok = false;
          try { ok = document.execCommand('copy'); } catch (e) {}
          document.body.removeChild(ta);
          done(ok);
        };
        if (navigator.clipboard && navigator.clipboard.writeText){
          navigator.clipboard.writeText(url).then(function(){ done(true); }, function(){ fallback(); });
        } else { fallback(); }
      });
    }
  }

  function addCopyButtons(){
    var targets = document.querySelectorAll('.code-block, .t-code');
    for (var i = 0; i < targets.length; i++){
      (function(t){
        var pre = t.querySelector('pre');
        if (!pre || t.querySelector('.copy-btn')) return;
        var btn = document.createElement('button');
        btn.type = 'button';
        btn.className = 'copy-btn';
        btn.textContent = 'Copy';
        btn.setAttribute('aria-label', 'Copy code');
        btn.addEventListener('click', function(){
          var text = pre.textContent;
          function done(ok){
            btn.textContent = ok ? 'Copied ✓' : 'Copy failed';
            setTimeout(function(){ btn.textContent = 'Copy'; }, 1600);
          }
          function fallback(){
            var ta = document.createElement('textarea');
            ta.value = text;
            ta.style.position = 'fixed';
            ta.style.opacity = '0';
            document.body.appendChild(ta);
            ta.select();
            var ok = false;
            try { ok = document.execCommand('copy'); } catch (e) {}
            document.body.removeChild(ta);
            done(ok);
          }
          if (navigator.clipboard && navigator.clipboard.writeText){
            navigator.clipboard.writeText(text).then(function(){ done(true); }, function(){ fallback(); });
          } else { fallback(); }
        });
        t.appendChild(btn);
      })(targets[i]);
    }
  }

  function bindPrint(){
    var printBtn = document.getElementById('print-btn');
    if (printBtn) printBtn.addEventListener('click', function(){ window.print(); });
    var saved = [];
    window.addEventListener('beforeprint', function(){
      saved = [];
      var ds = document.querySelectorAll('details');
      for (var i = 0; i < ds.length; i++){
        saved.push(ds[i].open);
        ds[i].open = true;
      }
    });
    window.addEventListener('afterprint', function(){
      var ds = document.querySelectorAll('details');
      for (var i = 0; i < ds.length; i++){
        if (saved[i] !== undefined) ds[i].open = saved[i];
      }
    });
  }

  var time = document.getElementById('meta-time');
  if (time && Number(time.dataset.ts) > 0) {
    time.textContent = new Date(Number(time.dataset.ts) * 1000).toLocaleString();
  }
  bindTabs();
  bindAnchors();
  addCopyButtons();
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

    fn tool_message() -> Message {
        Message {
            role: Role::Assistant,
            content: vec![
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
            ],
        }
    }

    fn doc() -> ShareDoc {
        ShareDoc {
            session: "proj-1700000000".into(),
            created_at: 1_700_000_000,
            agents: vec![AgentShare {
                name: "scout".into(),
                def: Some("scout".into()),
                description: "调研".into(),
                state: "idle".into(),
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
        let doc = doc();
        let html = render(
            &doc,
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
        assert!(html.contains("<figcaption>rust</figcaption>"), "语言标注");
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
        // 名字不同大概率不同色（散列）。
        let colors: std::collections::HashSet<&str> =
            ["dev", "qa", "ui-ux", "main", "scout", "worker"]
                .into_iter()
                .map(member_color)
                .collect();
        assert!(colors.len() >= 4, "成员色应分散：{colors:?}");
    }

    #[test]
    fn tool_args_extracts_first_meaningful_value() {
        assert_eq!(tool_args(&serde_json::json!({"command": "git status --short"})), "git status --short");
        assert_eq!(tool_args(&serde_json::json!({"file_path": "src/main.rs"})), "src/main.rs");
        assert_eq!(tool_args(&serde_json::json!({"pattern": "*.rs"})), "*.rs");
        assert_eq!(tool_args(&serde_json::json!({"query": "rust clap"})), "rust clap");
        // 超 60 字符截断加省略号。
        let long = "x".repeat(80);
        let args = tool_args(&serde_json::json!({"command": long}));
        assert!(args.ends_with('…'));
        assert_eq!(args.chars().count(), 61);
        // 无已知键：取对象首个字符串值；无字符串值 → 空。
        assert_eq!(tool_args(&serde_json::json!({"a": "b", "c": "d"})), "b");
        assert_eq!(tool_args(&serde_json::json!({"a": 1})), "");
    }

    #[test]
    fn renders_all_four_views() {
        let html = render(&doc(), &[text_msg(Role::User, "你好"), text_msg(Role::Assistant, "嗨")]);
        for view in ["view-conv", "view-team", "view-dm", "view-channel"] {
            assert!(html.contains(&format!("id=\"{view}\"")), "缺视图 {view}");
        }
        // 对话视图：装饰列 + 消息 id + 角色 meta + 无框 body / 卡。
        assert!(html.contains("你好") && html.contains("嗨"));
        assert!(html.contains("class=\"msg msg-user\"") && html.contains("class=\"msg msg-assistant\""));
        assert!(html.contains("id=\"msg-1\"") && html.contains("id=\"msg-2\""));
        assert!(html.contains("class=\"anchor\" href=\"#msg-1\""));
        assert!(html.contains("<div class=\"dec\""));
        assert!(html.contains("<span class=\"line\"></span>"));
        assert!(html.contains("<span class=\"role-user\">User</span>"));
        assert!(html.contains("<span class=\"role-assistant\">Assistant</span>"));
        assert!(html.contains("<div class=\"card\">"), "assistant 文本进细框卡");
        assert!(html.contains("<div class=\"content w-md\">"));
        // Team 视图：名册行。
        assert!(html.contains("<span class=\"r-name\">scout</span>"));
        assert!(html.contains("调研"));
        assert!(html.contains("class=\"r-state idle\""));
        assert!(html.contains("2 messages"));
        // 私聊视图：agent 线程。
        assert!(html.contains("<span class=\"a-name\">scout</span>"));
        assert!(html.contains("<strong>结论</strong>"));
        assert!(html.contains("查一下"));
        // 频道视图。
        assert!(html.contains("<span class=\"ch-name\">◇ #table</span>"), "频道头 ◇ 前缀（规格 §4.4）");
        assert!(html.contains("class=\"ch-mode free\""));
        assert!(html.contains(">main</span>") && html.contains(">user</span>") && html.contains(">scout</span>"));
        assert!(html.contains("<span class=\"ch-seq\">1</span>"));
        assert!(html.contains("大家好"));
    }

    #[test]
    fn tool_blocks_are_two_stage_and_escaped() {
        let html = render(&doc(), &[tool_message()]);
        // 两段式：tool-title（⏺ 名 + t-args 摘要）+ details.tool-result。
        assert!(html.contains("<div class=\"tool\">"), "工具两段式");
        assert!(html.contains("<span class=\"t-name\">Bash</span>"));
        assert!(html.contains("<span class=\"t-args\">ls &lt;unsafe&gt; &amp; echo &quot;x&quot;</span>"), "t-args 摘要转义");
        assert!(html.contains("<details class=\"tool-result w-sm\">"));
        assert!(html.contains("<summary>Show result</summary>"));
        assert!(html.contains("<span class=\"t-label\">input</span>"));
        assert!(html.contains("ls &lt;unsafe&gt;"), "tool_use 输入转义");
        assert!(html.contains("&amp; echo"), "输入内 & 转义");
        assert!(html.contains("src/ share.rs"));
        assert!(html.contains("<span class=\"t-status ok\">✓</span>"));
        // 错误结果有错误样式。
        let mut m = tool_message();
        m.content.push(ContentBlock::ToolResult {
            tool_use_id: "tu_2".into(),
            content: serde_json::Value::String("boom".into()),
            is_error: true,
        });
        let html = render(&doc(), &[m]);
        assert!(html.contains("class=\"t-code output err\""));
        assert!(html.contains("<span class=\"t-status err\">✗</span>"));
        assert!(html.contains("result (err)"));
    }

    #[test]
    fn thinking_is_expandable() {
        let m = Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Thinking {
                thinking: "深入思考一下".into(),
                signature: "sig".into(),
            }],
        };
        let html = render(&doc(), &[m]);
        assert!(html.contains("<details class=\"think\">"));
        assert!(html.contains("<summary>Thinking</summary>"));
        assert!(html.contains("深入思考一下"));
    }

    #[test]
    fn image_blocks_inline_as_data_uri() {
        let m = Message {
            role: Role::User,
            content: vec![ContentBlock::Image {
                source: crate::api::types::ImageSource::base64("image/png", "aGVsbG8="),
            }],
        };
        let html = render(&doc(), &[m]);
        assert!(html.contains("<figure class=\"img-block\">"), "{html}");
        assert!(html.contains("src=\"data:image/png;base64,aGVsbG8=\""));
        assert!(html.contains("alt=\"image (image/png)\""));
        assert!(html.contains("</figure>"));
        // 仅 data: URI，不透传外部 URL。
        assert!(!html.contains("http://") && !html.contains("https://"));
    }

    #[test]
    fn empty_views_show_hints() {
        let html = render(&ShareDoc::new("s".into()), &[]);
        // 四个视图各自的空态文案。
        let empty_count = html.matches("— No ").count();
        assert_eq!(empty_count, 4, "四视图空态：{html}");
        // 页脚隐私警示恒存在。
        assert!(html.contains("review before sharing"));
    }

    #[test]
    fn no_external_dependencies() {
        let html = render(&doc(), &[]);
        assert!(!html.contains("http://") && !html.contains("https://"), "无外链");
        assert!(!html.contains("<link"), "无外部样式表");
        assert!(!html.contains("src=\""), "无外部脚本/图片");
        assert!(!html.contains("@import"), "无 CSS import");
        assert!(!html.contains("<iframe"), "无 iframe");
    }
}
