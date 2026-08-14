use html5ever::{parse_document, tendril::TendrilSink};
use markup5ever_rcdom::{Handle, NodeData, RcDom};

const FENCE_3: &str = "\u{0060}\u{0060}\u{0060}";
const FENCE_4: &str = "\u{0060}\u{0060}\u{0060}\u{0060}";

#[derive(Clone, Copy, Default)]
struct Context {
    list_depth: usize,
    preformatted: bool,
}

pub(crate) fn convert(html: &str) -> String {
    let dom = parse_document(RcDom::default(), Default::default()).one(html);
    clean_markdown(render_children(&dom.document, Context::default()))
}

fn render_node(handle: &Handle, context: Context) -> String {
    match &handle.data {
        NodeData::Text { contents } => {
            let text = contents.borrow();
            if context.preformatted {
                text.to_string()
            } else {
                normalize_text(&text)
            }
        }
        NodeData::Element { name, attrs, .. } => {
            let tag = name.local.as_ref();
            match tag {
                "head" | "script" | "style" | "template" | "noscript" | "svg" => String::new(),
                "br" => "\n".to_string(),
                "hr" => "\n\n---\n\n".to_string(),
                "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                    let level = tag[1..].parse::<usize>().unwrap_or(1);
                    block(format!(
                        "{} {}",
                        "#".repeat(level),
                        compact(&render_children(handle, context))
                    ))
                }
                "p" | "div" | "section" | "article" | "main" | "header" | "footer" | "aside"
                | "nav" | "figure" | "figcaption" => block(render_children(handle, context)),
                "strong" | "b" => inline_wrap("**", render_children(handle, context)),
                "em" | "i" => inline_wrap("*", render_children(handle, context)),
                "del" | "s" | "strike" => inline_wrap("~~", render_children(handle, context)),
                "code" if !context.preformatted => {
                    let content = render_children(handle, context);
                    let fence = if content.contains(char::from(96u8)) {
                        "\u{0060}\u{0060}"
                    } else {
                        "\u{0060}"
                    };
                    format!("{fence}{}{fence}", content.trim())
                }
                "pre" => render_pre(handle),
                "a" => {
                    let label = compact(&render_children(handle, context));
                    let href = attribute(attrs, "href").unwrap_or_default();
                    if href.is_empty() {
                        label
                    } else if label.is_empty() || label == href {
                        format!("<{href}>")
                    } else {
                        format!("[{label}]({href})")
                    }
                }
                "img" => {
                    let source = attribute(attrs, "src").unwrap_or_default();
                    let alt = attribute(attrs, "alt").unwrap_or_default();
                    if source.is_empty() {
                        alt
                    } else {
                        format!("![{alt}]({source})")
                    }
                }
                "blockquote" => {
                    let content = clean_markdown(render_children(handle, context));
                    if content.is_empty() {
                        String::new()
                    } else {
                        block(
                            content
                                .lines()
                                .map(|line| format!("> {line}"))
                                .collect::<Vec<_>>()
                                .join("\n"),
                        )
                    }
                }
                "ul" => render_list(handle, context, false),
                "ol" => render_list(handle, context, true),
                "li" => render_children(handle, context),
                "table" => render_table(handle, context),
                "dl" => block(render_children(handle, context)),
                "dt" => block(inline_wrap("**", render_children(handle, context))),
                "dd" => block(format!(": {}", compact(&render_children(handle, context)))),
                _ => render_children(handle, context),
            }
        }
        _ => render_children(handle, context),
    }
}

fn render_children(handle: &Handle, context: Context) -> String {
    handle
        .children
        .borrow()
        .iter()
        .map(|child| render_node(child, context))
        .collect()
}

fn render_pre(handle: &Handle) -> String {
    let language = handle
        .children
        .borrow()
        .iter()
        .find_map(|child| match &child.data {
            NodeData::Element { name, attrs, .. } if name.local.as_ref() == "code" => {
                attribute(attrs, "class").and_then(|class| {
                    class
                        .split_whitespace()
                        .find_map(|item| item.strip_prefix("language-").map(str::to_string))
                })
            }
            _ => None,
        })
        .unwrap_or_default();
    let content = render_children(
        handle,
        Context {
            list_depth: 0,
            preformatted: true,
        },
    )
    .replace("\r\n", "\n");
    let content = content.strip_prefix('\n').unwrap_or(&content).trim_end();
    let fence = if content.contains(FENCE_3) {
        FENCE_4
    } else {
        FENCE_3
    };
    format!("\n\n{fence}{language}\n{content}\n{fence}\n\n")
}

fn render_list(handle: &Handle, context: Context, ordered: bool) -> String {
    let mut items = Vec::new();
    let indent = "  ".repeat(context.list_depth);
    let mut item_number = 0;
    for child in handle.children.borrow().iter() {
        if element_name(child) != Some("li") {
            continue;
        }
        item_number += 1;
        let marker = if ordered {
            format!("{item_number}. ")
        } else {
            "- ".to_string()
        };
        let mut content = String::new();
        let mut nested = String::new();
        for node in child.children.borrow().iter() {
            match element_name(node) {
                Some("ul") => nested.push_str(&render_list(
                    node,
                    Context {
                        list_depth: context.list_depth + 1,
                        ..context
                    },
                    false,
                )),
                Some("ol") => nested.push_str(&render_list(
                    node,
                    Context {
                        list_depth: context.list_depth + 1,
                        ..context
                    },
                    true,
                )),
                _ => content.push_str(&render_node(node, context)),
            }
        }
        let content = clean_markdown(content);
        let continuation = format!("{indent}{}", " ".repeat(marker.len()));
        let content = content
            .lines()
            .enumerate()
            .map(|(line, value)| {
                if line == 0 {
                    value.to_string()
                } else {
                    format!("{continuation}{value}")
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        items.push(format!("{indent}{marker}{content}{}", nested.trim_end()));
    }
    block(items.join("\n"))
}

fn render_table(handle: &Handle, context: Context) -> String {
    let mut rows = Vec::new();
    collect_rows(handle, context, &mut rows);
    if rows.is_empty() {
        return String::new();
    }
    let width = rows.iter().map(Vec::len).max().unwrap_or(0);
    if width == 0 {
        return String::new();
    }
    for row in &mut rows {
        row.resize(width, String::new());
    }
    let mut output = format!("| {} |\n", rows[0].join(" | "));
    output.push_str(&format!("| {} |", vec!["---"; width].join(" | ")));
    for row in rows.iter().skip(1) {
        output.push_str(&format!("\n| {} |", row.join(" | ")));
    }
    block(output)
}

fn collect_rows(handle: &Handle, context: Context, rows: &mut Vec<Vec<String>>) {
    if element_name(handle) == Some("tr") {
        let cells = handle
            .children
            .borrow()
            .iter()
            .filter(|child| matches!(element_name(child), Some("th" | "td")))
            .map(|cell| compact(&render_children(cell, context)).replace('|', "\\|"))
            .collect::<Vec<_>>();
        if !cells.is_empty() {
            rows.push(cells);
        }
        return;
    }
    for child in handle.children.borrow().iter() {
        collect_rows(child, context, rows);
    }
}

fn element_name(handle: &Handle) -> Option<&str> {
    match &handle.data {
        NodeData::Element { name, .. } => Some(name.local.as_ref()),
        _ => None,
    }
}

fn attribute(attrs: &std::cell::RefCell<Vec<html5ever::Attribute>>, name: &str) -> Option<String> {
    attrs
        .borrow()
        .iter()
        .find(|attribute| attribute.name.local.as_ref() == name)
        .map(|attribute| attribute.value.to_string())
}

fn inline_wrap(delimiter: &str, value: String) -> String {
    let value = value.trim();
    if value.is_empty() {
        String::new()
    } else {
        format!("{delimiter}{value}{delimiter}")
    }
}

fn block(value: String) -> String {
    let value = value.trim();
    if value.is_empty() {
        String::new()
    } else {
        format!("\n\n{value}\n\n")
    }
}

fn compact(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn normalize_text(value: &str) -> String {
    let leading = value.chars().next().is_some_and(char::is_whitespace);
    let trailing = value.chars().next_back().is_some_and(char::is_whitespace);
    let content = compact(value);
    if content.is_empty() {
        return " ".to_string();
    }
    format!(
        "{}{}{}",
        if leading { " " } else { "" },
        content,
        if trailing { " " } else { "" }
    )
}

fn clean_markdown(value: String) -> String {
    let value = value.replace("\r\n", "\n").replace('\r', "\n");
    let mut lines = Vec::new();
    let mut blank = false;
    let mut in_fence = false;
    for raw in value.lines() {
        let line = raw.trim_end();
        if line.trim_start().starts_with(FENCE_3) {
            in_fence = !in_fence;
        }
        if !in_fence && line.trim().is_empty() {
            if blank {
                continue;
            }
            blank = true;
            lines.push(String::new());
        } else {
            blank = false;
            lines.push(line.to_string());
        }
    }
    lines.join("\n").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_structured_html_without_active_content() {
        let markdown = convert(
            "<html><head><script>steal()</script></head><body><h1>Docs &amp; help</h1><p>Read <strong>carefully</strong> at <a href=\"https://example.com\">the guide</a>.</p><ul><li>First</li><li>Second</li></ul></body></html>",
        );
        assert!(markdown.contains("# Docs & help"));
        assert!(markdown.contains("Read **carefully** at [the guide](https://example.com)."));
        assert!(markdown.contains("- First\n- Second"));
        assert!(!markdown.contains("steal"));
    }

    #[test]
    fn preserves_code_blocks_and_tables() {
        let markdown = convert(
            "<pre><code class=\"language-rust\">fn main() {\n  println!(\"ok\");\n}</code></pre><table><tr><th>Name</th><th>Value</th></tr><tr><td>A</td><td>1</td></tr></table>",
        );
        assert!(markdown.contains(&format!(
            "{FENCE_3}rust\nfn main() {{\n  println!(\"ok\");\n}}\n{FENCE_3}"
        )));
        assert!(markdown.contains("| Name | Value |\n| --- | --- |\n| A | 1 |"));
    }
}
