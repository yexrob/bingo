//! share 页面的 HTML 渲染（`bingo share` 输出）。
//!
//! 产物是自包含单文件：CSS/JS 内嵌、零外部依赖、离线可用。视觉与组件类
//! 对齐 `notes/design/share-page-template.html`（ui-ux 设计事实源），数据
//! 在 Rust 侧服务端渲染：所有动态文本先经 [`escape`]（`& < > " '` 全量转义）
//! 再拼进 HTML，JS 只做 tab 切换/打印/时间格式化，不拼接任何数据——不存在
//! 脚本注入面。文本块走最小 markdown→HTML（标题/粗体/行内代码/代码块/列表），
//! 不做完整 md 引擎。

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

/// 成员取色：main/assistant 恒 accent，user 恒白，其余按名字 FNV 哈希取
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

/// 消息内容块 → 正文 HTML（tool_use/tool_result 折叠、thinking 可展开）。
fn render_blocks(blocks: &[ContentBlock]) -> String {
    let mut body = String::new();
    for block in blocks {
        match block {
            ContentBlock::Text { text } => body.push_str(&render_markdown(text)),
            ContentBlock::Thinking { thinking, .. } => {
                body.push_str(&format!(
                    "<details class=\"think\"><summary>思考</summary><div class=\"think-body\">{}</div></details>",
                    render_markdown(thinking)
                ));
            }
            ContentBlock::ToolUse { name, input, .. } => {
                let pretty = serde_json::to_string_pretty(input).unwrap_or_default();
                body.push_str(&format!(
                    "<details class=\"tool-run\"><summary><span class=\"t-ico\">⏺</span><span class=\"t-name\">{}</span></summary><div class=\"t-body\"><div class=\"t-code\"><span class=\"t-label\">input</span><pre>{}</pre></div></div></details>",
                    escape(name),
                    escape(&pretty)
                ));
            }
            ContentBlock::ToolResult { content, is_error, .. } => {
                let (status_cls, status, label, err_cls) = if *is_error {
                    ("err", "err", "result (err)", " err")
                } else {
                    ("ok", "ok", "result", "")
                };
                body.push_str(&format!(
                    "<details class=\"tool-run\"><summary><span class=\"t-ico\">⏺</span><span class=\"t-name\">tool_result</span><span class=\"t-status {status_cls}\">{status}</span></summary><div class=\"t-body\"><div class=\"t-code output{err_cls}\"><span class=\"t-label\">{label}</span><pre>{}</pre></div></div></details>",
                    escape(&tool_result_text(content))
                ));
            }
            ContentBlock::Image { .. } => {
                body.push_str(
                    "<details class=\"tool-run\"><summary><span class=\"t-ico\">⏺</span><span class=\"t-name\">image</span></summary><div class=\"t-body\"><p>（图片内容，share 页面不内联展示）</p></div></details>",
                );
            }
        }
    }
    body
}

/// 单条主对话消息（用户/助手）。
fn render_message(msg: &Message) -> String {
    let (who, class, role_var) = match msg.role {
        Role::User => ("你", "msg-user", "var(--text)"),
        Role::Assistant => ("助手", "msg-assistant", "var(--accent)"),
    };
    format!(
        "<div class=\"msg {class}\" style=\"--role:{role_var}\"><div class=\"msg-head\"><span class=\"role\">{who}</span></div><div class=\"msg-body\">{}</div></div>",
        render_blocks(&msg.content)
    )
}

fn render_messages(messages: &[Message]) -> String {
    if messages.is_empty() {
        return "<div class=\"empty\">— 无记录 —</div>".to_string();
    }
    let mut out = String::from("<div class=\"conv\">");
    for m in messages {
        out.push_str(&render_message(m));
    }
    out.push_str("</div>");
    out
}

fn render_team(agents: &[AgentShare]) -> String {
    if agents.is_empty() {
        return "<div class=\"empty\">— 无记录 —</div>".to_string();
    }
    let mut out = String::from("<div class=\"roster\">");
    for a in agents {
        let def = a.def.as_deref().map(escape).unwrap_or_else(|| "—".to_string());
        out.push_str(&format!(
            "<div class=\"r-row\"><span class=\"r-state {}\">● {}</span><span class=\"r-name\">{}</span><span class=\"r-meta\">{} · {} 条消息</span><span class=\"r-desc\">{}</span></div>",
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
        return "<div class=\"empty\">— 无记录 —</div>".to_string();
    }
    let mut out = String::from("<div class=\"dm-list\">");
    for a in agents {
        let mut thread = String::new();
        if a.history.is_empty() {
            thread.push_str("<div class=\"dm-thread\"><p class=\"empty\">（暂无历史）</p></div>");
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
                thread.push_str(&format!(
                    "<div class=\"dm-row{user_cls}\"><span class=\"dm-from\" style=\"--from:{from_color}\">{}</span><span class=\"dm-text\">{}</span><span class=\"dm-time\"></span></div>",
                    escape(from),
                    render_blocks(&m.content)
                ));
            }
            thread.push_str("</div>");
        }
        out.push_str(&format!(
            "<details class=\"dm-agent\"><summary><span class=\"a-dot\" style=\"--dot:{}\"></span><span class=\"a-name\">{}</span><span class=\"a-state\">{}</span><span class=\"a-count\">{} 条</span></summary>{}</details>",
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
        return "<div class=\"empty\">— 无记录 —</div>".to_string();
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
            stream.push_str("<p class=\"empty\">（暂无消息）</p>");
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
            "<div class=\"ch-block\"><div class=\"ch-head\"><span class=\"ch-name\">#{}</span><span class=\"ch-mode {}\">{}</span><span class=\"ch-members\">{}</span></div>{}</div>",
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
    out.push_str("<!DOCTYPE html>\n<html lang=\"zh-CN\">\n<head>\n<meta charset=\"utf-8\">\n");
    out.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    out.push_str("<meta name=\"color-scheme\" content=\"dark light\">\n");
    out.push_str("<meta name=\"generator\" content=\"bingo share\">\n");
    out.push_str(&format!("<title>bingo · {session}</title>\n"));
    out.push_str("<style>\n");
    out.push_str(CSS);
    out.push_str("</style>\n</head>\n<body>\n");
    out.push_str("<a class=\"skip\" href=\"#main\">跳到内容</a>\n");
    out.push_str("<header class=\"topbar\"><div class=\"topbar-inner\"><div class=\"brand\">");
    out.push_str("<span class=\"mark\">▸</span><span class=\"name\">bingo</span>");
    out.push_str(&format!("<span class=\"session\">{session}</span>"));
    out.push_str(&format!(
        "<div class=\"meta-line\"><span class=\"meta-item\" id=\"meta-time\" data-ts=\"{}\"></span>",
        doc.created_at
    ));
    out.push_str("<button type=\"button\" class=\"print-btn\" id=\"print-btn\" aria-label=\"打印此页面\">⎙ 打印</button></div>");
    out.push_str("</div><nav class=\"tabs\" role=\"tablist\" aria-label=\"视图切换\">");
    out.push_str("<button role=\"tab\" data-tab=\"conv\" aria-selected=\"true\"><span class=\"kbd\">[1]</span>对话</button>");
    out.push_str(&format!(
        "<button role=\"tab\" data-tab=\"team\" aria-selected=\"false\"><span class=\"kbd\">[2]</span>Team <span class=\"count\">{}</span></button>",
        doc.agents.len()
    ));
    out.push_str(&format!(
        "<button role=\"tab\" data-tab=\"dm\" aria-selected=\"false\"><span class=\"kbd\">[3]</span>私聊 <span class=\"count\">{}</span></button>",
        doc.agents.len()
    ));
    out.push_str(&format!(
        "<button role=\"tab\" data-tab=\"channel\" aria-selected=\"false\"><span class=\"kbd\">[4]</span>频道 <span class=\"count\">{}</span></button>",
        doc.channels.len()
    ));
    out.push_str("</nav></div></header>\n<main id=\"main\">\n");
    out.push_str(&format!(
        "<section class=\"view\" data-view=\"conv\" role=\"tabpanel\" id=\"view-conv\" aria-label=\"对话\"><h2>对话 <span class=\"n\">· {} 条消息</span></h2>{}</section>\n",
        messages.len(),
        render_messages(messages)
    ));
    out.push_str(&format!(
        "<section class=\"view\" data-view=\"team\" role=\"tabpanel\" id=\"view-team\" aria-label=\"Team 名册\" hidden><h2>Team 名册 <span class=\"n\">· {} 个实例</span></h2>{}</section>\n",
        doc.agents.len(),
        render_team(&doc.agents)
    ));
    out.push_str(&format!(
        "<section class=\"view\" data-view=\"dm\" role=\"tabpanel\" id=\"view-dm\" aria-label=\"私聊\" hidden><h2>私聊 <span class=\"n\">· 每实例一段完整历史</span></h2>{}</section>\n",
        render_private(&doc.agents)
    ));
    out.push_str(&format!(
        "<section class=\"view\" data-view=\"channel\" role=\"tabpanel\" id=\"view-channel\" aria-label=\"频道\" hidden><h2>频道 <span class=\"n\">· {} 个房间</span></h2>{}</section>\n",
        doc.channels.len(),
        render_channels(&doc.channels)
    ));
    out.push_str("</main>\n<footer><div class=\"foot-inner\"><span><span class=\"mark\">▸</span> bingo · Rust agent CLI</span><span>由 <span class=\"mark\">bingo share</span> 生成</span></div></footer>\n");
    out.push_str("<noscript><div style=\"padding:24px;text-align:center;color:var(--faint)\">此分享页的视图切换需要 JavaScript；无 JS 时默认显示对话视图。</div></noscript>\n");
    out.push_str("<script>\n");
    out.push_str(JS);
    out.push_str("</script>\n</body>\n</html>\n");
    out
}

/// 内嵌样式：与 notes/design/share-page-template.html 同源（ui-ux 设计事实源）。
const CSS: &str = r#"
:root{
  --bg:#0C0C0E; --bg-panel:#121215; --bg-code:#161619; --bg-user:#2A2A30; --bg-hover:#1A1A1E;
  --hairline:#26262B; --hairline-strong:#33333A;
  --text:#E8E8E6; --dim:#A6A6A8; --faint:#7A7A80; --ink:#0C0C0E;
  --accent:#D77757; --teal:#4FB3C7; --green:#4EBA65; --red:#FF6B80; --gold:#FFC107;
  --hue-0:#48A39C; --hue-1:#B1B9F9; --hue-2:#AF87FF; --hue-3:#D9A441; --hue-4:#FD5DB1; --hue-5:#6CCB82;
  --font-mono:ui-monospace,"SF Mono","JetBrains Mono","Cascadia Code",Menlo,Consolas,"Liberation Mono","DejaVu Sans Mono",monospace;
  --fs:14px; --fs-sm:13px; --fs-xs:12px; --lh:1.65;
  --s1:4px; --s2:8px; --s3:12px; --s4:16px; --s5:24px; --s6:32px; --s7:48px;
  --radius:6px;
  --maxw:880px;
}
*,*::before,*::after{box-sizing:border-box}
html{-webkit-text-size-adjust:100%}
body{margin:0;background:var(--bg);color:var(--text);
  font-family:var(--font-mono);font-size:var(--fs);line-height:var(--lh);
  -webkit-font-smoothing:antialiased}
[hidden]{display:none!important}
::selection{background:var(--accent);color:var(--ink)}
a{color:var(--accent)}
a:hover{text-decoration-thickness:2px}
:focus-visible{outline:2px solid var(--accent);outline-offset:2px;border-radius:2px}
.skip{position:absolute;left:var(--s4);top:var(--s4);z-index:50;padding:var(--s2) var(--s3);
  background:var(--accent);color:var(--ink);text-decoration:none;border-radius:var(--radius);
  transform:translateY(-300%);transition:transform .12s}
.skip:focus{transform:none}
.topbar{position:sticky;top:0;z-index:10;background:var(--bg);border-bottom:1px solid var(--hairline)}
.topbar-inner{max-width:var(--maxw);margin:0 auto;padding:0 var(--s4)}
.brand{display:flex;align-items:baseline;gap:var(--s2);padding:var(--s3) 0 var(--s2)}
.brand .mark{color:var(--accent);font-weight:700}
.brand .name{font-weight:700;letter-spacing:.02em}
.brand .session{color:var(--dim);font-size:var(--fs-sm);overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.brand .session::before{content:"· ";color:var(--faint)}
.meta-line{display:flex;align-items:center;gap:var(--s3);margin-left:auto;color:var(--faint);font-size:var(--fs-xs);white-space:nowrap;overflow:hidden}
.meta-line .meta-item{overflow:hidden;text-overflow:ellipsis}
.print-btn{appearance:none;border:1px solid var(--hairline-strong);background:transparent;color:var(--dim);
  font:inherit;font-size:var(--fs-xs);padding:2px 8px;border-radius:var(--radius);cursor:pointer}
.print-btn:hover{color:var(--text);border-color:var(--accent)}
.tabs{display:flex;gap:0;border-top:1px solid var(--hairline)}
.tabs button{appearance:none;background:transparent;border:0;border-bottom:2px solid transparent;
  color:var(--faint);font:inherit;font-size:var(--fs-sm);padding:var(--s2) var(--s4) calc(var(--s2) + 1px);
  cursor:pointer;position:relative;top:-1px}
.tabs button:hover{color:var(--dim)}
.tabs button[aria-selected="true"]{color:var(--text);border-bottom-color:var(--accent)}
.tabs .count{color:var(--faint);font-size:var(--fs-xs);margin-left:2px}
.tabs button[aria-selected="true"] .count{color:var(--accent)}
.tabs .kbd{color:var(--faint);margin-right:var(--s1)}
main{max-width:var(--maxw);margin:0 auto;padding:var(--s5) var(--s4) var(--s7)}
.view[data-view]{animation:view-in .12s ease-out}
@keyframes view-in{from{opacity:0;transform:translateY(6px)}to{opacity:1;transform:none}}
.view h2{margin:0 0 var(--s4);font-size:var(--fs);font-weight:700;color:var(--dim)}
.view h2 .n{color:var(--faint);font-weight:400}
.empty{color:var(--faint);padding:var(--s6) 0;text-align:center}
.conv{position:relative;padding-left:24px}
.conv::before{content:"";position:absolute;left:6px;top:8px;bottom:8px;width:1px;background:var(--hairline)}
.msg{position:relative;margin-bottom:var(--s5)}
.msg::before{content:"";position:absolute;left:-24px;top:6px;width:9px;height:9px;transform:rotate(45deg);
  background:var(--role,var(--dim));border-radius:2px}
.msg-head{display:flex;align-items:baseline;gap:var(--s2);margin-bottom:var(--s2)}
.msg-head .role{font-weight:700}
.msg-head time{color:var(--faint);font-size:var(--fs-xs)}
.msg-head .model-tag{color:var(--faint);font-size:var(--fs-xs);margin-left:auto}
.msg-user .msg-head .role{color:var(--text)}
.msg-assistant .msg-head .role{color:var(--accent)}
.msg-user .msg-body{background:var(--bg-user);border-radius:var(--radius);padding:var(--s3) var(--s4)}
.msg-body>:first-child{margin-top:0}
.msg-body>:last-child{margin-bottom:0}
.msg-body h1,.msg-body h2,.msg-body h3,.msg-body h4,.msg-body h5,.msg-body h6{
  margin:var(--s4) 0 var(--s2);line-height:1.4}
.msg-body h1{font-size:1.3em;border-bottom:1px solid var(--hairline);padding-bottom:var(--s1)}
.msg-body h2{font-size:1.15em}.msg-body h3{font-size:1.05em}
.msg-body p{margin:var(--s2) 0}
.msg-body ul,.msg-body ol{margin:var(--s2) 0;padding-left:var(--s5)}
.msg-body li{margin:2px 0}
.msg-body code{background:var(--bg-code);border:1px solid var(--hairline);border-radius:4px;
  padding:0 4px;font-size:.92em;color:var(--teal)}
.msg-body pre code{background:none;border:0;padding:0;color:inherit;font-size:inherit}
.msg-body blockquote{margin:var(--s3) 0;padding-left:var(--s4);border-left:3px solid var(--hairline-strong);color:var(--dim)}
.msg-body hr{border:0;border-top:1px solid var(--hairline);margin:var(--s4) 0}
.msg-body table{border-collapse:collapse;margin:var(--s3) 0;font-size:var(--fs-sm);width:100%}
.msg-body th,.msg-body td{border:1px solid var(--hairline);padding:4px var(--s3);text-align:left}
.msg-body th{background:var(--bg-panel);font-weight:700}
.msg-body del{color:var(--faint)}
.code-block{margin:var(--s3) 0;background:var(--bg-code);border:1px solid var(--hairline);border-radius:var(--radius);position:relative}
.code-block figcaption{position:absolute;top:6px;right:12px;color:var(--faint);font-size:var(--fs-xs)}
.code-block pre{margin:0;padding:var(--s3) var(--s4);overflow-x:auto;font-size:var(--fs-sm);line-height:1.6;color:#B4B4B9}
details.think{margin:var(--s2) 0;border-left:2px solid var(--hairline);padding-left:var(--s3)}
details.think summary{cursor:pointer;color:var(--faint);font-style:italic;font-size:var(--fs-sm);
  list-style:none;display:flex;align-items:center;gap:var(--s2)}
details.think summary::-webkit-details-marker{display:none}
details.think summary::before{content:"∴";color:var(--faint)}
details.think summary:hover{color:var(--dim)}
details.think[open] summary{margin-bottom:var(--s2)}
.think-body{color:var(--dim);font-style:italic;font-size:var(--fs-sm);white-space:pre-wrap}
details.tool-run{margin:var(--s2) 0;border:1px solid var(--hairline);border-radius:var(--radius);background:var(--bg-panel)}
details.tool-run summary{cursor:pointer;padding:6px var(--s3);list-style:none;display:flex;align-items:center;gap:var(--s2);
  font-size:var(--fs-sm)}
details.tool-run summary::-webkit-details-marker{display:none}
details.tool-run summary .t-ico{color:var(--teal)}
details.tool-run summary .t-name{font-weight:700;color:var(--teal)}
details.tool-run summary .t-args{color:var(--faint);overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
details.tool-run summary .t-status{margin-left:auto;color:var(--faint);white-space:nowrap}
details.tool-run summary .t-status.ok{color:var(--green)}
details.tool-run summary .t-status.err{color:var(--red)}
details.tool-run summary:hover{background:var(--bg-hover)}
details.tool-run .t-body{padding:0 var(--s3) var(--s3);display:grid;gap:var(--s2)}
.t-code{position:relative;background:var(--bg-code);border:1px solid var(--hairline);border-radius:4px}
.t-code .t-label{display:block;padding:4px var(--s3);color:var(--faint);font-size:var(--fs-xs);border-bottom:1px solid var(--hairline)}
.t-code pre{margin:0;padding:var(--s2) var(--s3);overflow-x:auto;font-size:var(--fs-sm);line-height:1.6;color:#B4B4B9;white-space:pre-wrap;word-break:break-word}
.t-code.output pre{color:var(--dim)}
.t-code.output.err{border-color:rgba(255,107,128,.4)}
.t-code.output.err .t-label{color:var(--red)}
.roster{border:1px solid var(--hairline);border-radius:var(--radius)}
.r-row{display:grid;grid-template-columns:56px 120px 1fr;gap:var(--s2) var(--s3);
  padding:var(--s3) var(--s4);align-items:baseline}
.r-row+.r-row{border-top:1px solid var(--hairline)}
.r-row:hover{background:var(--bg-hover)}
.r-state{display:flex;align-items:center;gap:var(--s1);color:var(--faint);font-size:var(--fs-sm);white-space:nowrap}
.r-state.running{color:var(--teal)}
.r-state.stopped{color:var(--red)}
.r-name{color:var(--accent);font-weight:700;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.r-meta{color:var(--faint);font-size:var(--fs-xs);white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
.r-desc{color:var(--dim);font-size:var(--fs-sm);grid-column:2 / -1;margin-top:-6px}
.dm-list{display:grid;gap:var(--s2)}
.dm-agent{border:1px solid var(--hairline);border-radius:var(--radius);background:var(--bg-panel)}
.dm-agent summary{cursor:pointer;padding:var(--s2) var(--s4);list-style:none;display:flex;align-items:center;gap:var(--s3)}
.dm-agent summary::-webkit-details-marker{display:none}
.dm-agent summary .a-dot{width:8px;height:8px;border-radius:50%;background:var(--dot,var(--faint))}
.dm-agent summary .a-name{font-weight:700}
.dm-agent summary .a-state{color:var(--faint);font-size:var(--fs-xs)}
.dm-agent summary .a-count{margin-left:auto;color:var(--faint);font-size:var(--fs-xs)}
.dm-agent summary:hover{background:var(--bg-hover)}
.dm-agent[open] summary{border-bottom:1px solid var(--hairline)}
.dm-thread{padding:var(--s2) 0}
.dm-row{display:grid;grid-template-columns:90px 1fr auto;gap:var(--s3);padding:var(--s1) var(--s4);align-items:baseline}
.dm-row .dm-from{color:var(--from,var(--accent));font-weight:700;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.dm-row .dm-text{color:var(--text);white-space:pre-wrap;word-break:break-word}
.dm-row .dm-time{color:var(--faint);font-size:var(--fs-xs);white-space:nowrap}
.dm-row.dm-user .dm-from{color:var(--text)}
.ch-list{display:grid;gap:var(--s4)}
.ch-block{border:1px solid var(--hairline);border-radius:var(--radius);background:var(--bg-panel)}
.ch-head{display:flex;align-items:center;gap:var(--s3);flex-wrap:wrap;padding:var(--s3) var(--s4);border-bottom:1px solid var(--hairline)}
.ch-head .ch-name{font-weight:700;font-size:var(--fs)}
.ch-mode{font-size:var(--fs-xs);padding:1px 8px;border:1px solid;border-radius:999px}
.ch-mode.serial{color:var(--teal);border-color:rgba(79,179,199,.45);background:rgba(79,179,199,.08)}
.ch-mode.free{color:var(--hue-1);border-color:rgba(177,185,249,.45);background:rgba(177,185,249,.08)}
.ch-members{margin-left:auto;display:flex;gap:var(--s1);flex-wrap:wrap}
.m-chip{font-size:var(--fs-xs);color:var(--dim);display:inline-flex;align-items:center;gap:4px}
.m-chip::before{content:"";width:6px;height:6px;border-radius:50%;background:var(--chip,var(--faint))}
.ch-stream{padding:var(--s2) 0}
.ch-row{display:grid;grid-template-columns:56px 100px 1fr;gap:var(--s3);padding:var(--s1) var(--s4);align-items:baseline}
.ch-row .ch-seq{color:var(--faint);font-size:var(--fs-xs);text-align:right}
.ch-row .ch-from{color:var(--from,var(--accent));font-weight:700;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.ch-row .ch-text{color:var(--text);white-space:pre-wrap;word-break:break-word}
.ch-row.ch-user{grid-template-columns:1fr 100px 56px}
.ch-row.ch-user .ch-text{text-align:right;color:var(--text)}
.ch-row.ch-user .ch-seq{order:3}
.ch-row.ch-user .ch-from{order:2}
.ch-row.ch-user .ch-text{order:1}
.ch-row+.ch-row{border-top:1px dashed var(--hairline)}
footer{border-top:1px solid var(--hairline);color:var(--faint);font-size:var(--fs-xs)}
.foot-inner{max-width:var(--maxw);margin:0 auto;padding:var(--s3) var(--s4);display:flex;gap:var(--s3);flex-wrap:wrap}
.foot-inner .mark{color:var(--accent)}
@media (max-width:640px){
  .meta-line{display:none}
  .r-row{grid-template-columns:56px 1fr;grid-template-areas:"state name" "state meta" "desc desc"}
  .r-state{grid-area:state}.r-name{grid-area:name}.r-meta{grid-area:meta}.r-desc{grid-area:desc;margin-top:0}
  .dm-row{grid-template-columns:72px 1fr}
  .dm-row .dm-time{display:none}
  .ch-row{grid-template-columns:44px 76px 1fr}
  .ch-row.ch-user{grid-template-columns:1fr 76px 44px}
  .brand .session{max-width:40vw}
}
@media print{
  :root{
    --bg:#FFFFFF; --bg-panel:#FFFFFF; --bg-code:#F5F5F6; --bg-user:#F0F0F2; --bg-hover:#FFFFFF;
    --hairline:#DDDDDD; --hairline-strong:#BBBBBB;
    --text:#1A1A1E; --dim:#4A4A50; --faint:#66666C; --ink:#FFFFFF;
    --accent:#B05227; --teal:#24707F; --green:#2E7D3F; --red:#C22E47; --gold:#8A6508;
    --hue-0:#2A6F69; --hue-1:#5A67C9; --hue-2:#6A4DB8; --hue-3:#8A6418; --hue-4:#A93F7D; --hue-5:#3E7A4C;
  }
  .topbar{position:static}
  .tabs,.print-btn,.skip{display:none!important}
  .msg-body code{border:0;color:var(--teal)}
  details{display:block!important}
  details:not([open]) summary{display:block;color:var(--text)!important;font-style:normal!important}
  .code-block pre,.t-code pre{white-space:pre-wrap;word-break:break-word;color:#333}
  pre,details summary,figcaption{break-inside:avoid}
  .msg{break-inside:avoid}
  a{text-decoration:underline}
  body{font-size:12px}
}
@media (prefers-reduced-motion:reduce){
  *{transition:none!important;animation:none!important}
}
"#;

/// 视图切换 + 打印 + 时间格式化。不拼接任何会话数据（防注入）。
const JS: &str = r#"
(function(){
  'use strict';
  var tabs = Array.prototype.slice.call(document.querySelectorAll('.tabs button[data-tab]'));
  var views = Array.prototype.slice.call(document.querySelectorAll('.view[data-view]'));
  function show(name){
    tabs.forEach(function(t){ t.setAttribute('aria-selected', String(t.dataset.tab === name)); });
    views.forEach(function(v){ v.hidden = (v.dataset.view !== name); });
  }
  tabs.forEach(function(t){ t.addEventListener('click', function(){ show(t.dataset.tab); }); });
  var printBtn = document.getElementById('print-btn');
  if (printBtn) { printBtn.addEventListener('click', function(){ window.print(); }); }
  var time = document.getElementById('meta-time');
  if (time && Number(time.dataset.ts) > 0) {
    time.textContent = '生成于 ' + new Date(Number(time.dataset.ts) * 1000).toLocaleString();
  }
  document.addEventListener('keydown', function(e){
    if (e.altKey || e.ctrlKey || e.metaKey || e.shiftKey) { return; }
    var map = { '1':'conv', '2':'team', '3':'dm', '4':'channel' };
    if (map[e.key]) { show(map[e.key]); }
  });
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
        // 注入内容经 render 全链路不出现可执行脚本。
        let doc = doc();
        let html = render(
            &doc,
            &[text_msg(Role::User, "<script>alert(1)</script>")],
        );
        assert!(!html.contains("<script>alert(1)"), "注入脚本不得原样出现");
        assert!(html.contains("&lt;script&gt;"), "{html}");
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
    fn renders_all_four_views() {
        let html = render(&doc(), &[text_msg(Role::User, "你好"), text_msg(Role::Assistant, "嗨")]);
        for view in ["view-conv", "view-team", "view-dm", "view-channel"] {
            assert!(html.contains(&format!("id=\"{view}\"")), "缺视图 {view}");
        }
        // 对话视图内容（含角色样式）。
        assert!(html.contains("你好") && html.contains("嗨"));
        assert!(html.contains("msg-user") && html.contains("msg-assistant"));
        // Team 视图：名册行（名字/定义/描述/状态/消息数）。
        assert!(html.contains("<span class=\"r-name\">scout</span>"));
        assert!(html.contains("调研"));
        assert!(html.contains("class=\"r-state idle\""));
        assert!(html.contains("2 条消息"));
        // 私聊视图：agent 线程（名 + 历史 markdown）。
        assert!(html.contains("<span class=\"a-name\">scout</span>"));
        assert!(html.contains("<strong>结论</strong>"));
        assert!(html.contains("查一下"));
        // 频道视图：名字/模式/成员 chip/消息流。
        assert!(html.contains("<span class=\"ch-name\">#table</span>"));
        assert!(html.contains("class=\"ch-mode free\""));
        assert!(html.contains(">main</span>") && html.contains(">user</span>") && html.contains(">scout</span>"));
        assert!(html.contains("<span class=\"ch-seq\">1</span>"));
        assert!(html.contains("大家好"));
    }

    #[test]
    fn tool_blocks_render_collapsed_and_escaped() {
        let html = render(&doc(), &[tool_message()]);
        assert!(html.contains("<details class=\"tool-run\">"), "tool_use 折叠");
        assert!(html.contains("<span class=\"t-name\">Bash</span>"));
        assert!(html.contains("ls &lt;unsafe&gt;"), "tool_use 输入转义");
        assert!(html.contains("&amp; echo"), "输入内 & 转义");
        assert!(html.contains("src/ share.rs"));
        assert!(html.contains("<span class=\"t-label\">result</span>"));
        assert!(html.contains("<span class=\"t-status ok\">ok</span>"));
        // 错误结果有错误样式。
        let mut m = tool_message();
        m.content.push(ContentBlock::ToolResult {
            tool_use_id: "tu_2".into(),
            content: serde_json::Value::String("boom".into()),
            is_error: true,
        });
        let html = render(&doc(), &[m]);
        assert!(html.contains("class=\"t-code output err\""));
        assert!(html.contains("<span class=\"t-status err\">err</span>"));
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
        assert!(html.contains("<summary>思考</summary>"));
        assert!(html.contains("深入思考一下"));
    }

    #[test]
    fn empty_views_show_hints() {
        let html = render(&ShareDoc::new("s".into()), &[]);
        // 四个视图各自的空态文案（对话/Team/私聊/频道）。
        let empty_count = html.matches("— 无记录 —").count();
        assert_eq!(empty_count, 4, "四视图空态：{html}");
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
