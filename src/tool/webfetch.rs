use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;

use super::{parse_input, Tool, ToolContext, ToolError, ToolResult};

/// WebFetch 请求超时（连接 + 读取）。
const FETCH_TIMEOUT: Duration = Duration::from_secs(30);
/// 抓取结果上限（超过截断）。
const MAX_FETCH_CHARS: usize = 100_000;

#[derive(Debug, Deserialize)]
pub struct WebFetchInput {
    pub url: String,
}

/// WebFetch：抓取 URL 并转纯文本（对标 Claude Code WebFetchTool，无 readability 的简化版）。
pub struct WebFetchTool;

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> String {
        "WebFetch".into()
    }
    fn description(&self) -> String {
        "Fetch a URL and return its content as plain text (HTML tags stripped).".into()
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "url": {"type": "string"}
            },
            "required": ["url"]
        })
    }
    fn is_concurrency_safe(&self, _input: &serde_json::Value) -> bool {
        true
    }
    fn is_read_only(&self, _input: &serde_json::Value) -> bool {
        true
    }
    async fn call(
        &self,
        input: serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let params: WebFetchInput = parse_input(&input)?;
        let client = reqwest::Client::new();
        let response = tokio::time::timeout(FETCH_TIMEOUT, async {
            client
                .get(&params.url)
                .header("User-Agent", "bingo-agent/0.1 (agent cli)")
                .send()
                .await
        })
        .await
        .map_err(|_| ToolError::failed(format!("fetch timed out: {}", params.url)))?;
        let response = response.map_err(|e| ToolError::failed(format!("fetch failed: {e}")))?;
        if !response.status().is_success() {
            return Err(ToolError::failed(format!(
                "fetch failed: HTTP {} for {}",
                response.status(),
                params.url
            )));
        }
        let bytes = tokio::time::timeout(FETCH_TIMEOUT, response.bytes())
            .await
            .map_err(|_| ToolError::failed("fetch body timed out".to_string()))?
            .map_err(|e| ToolError::failed(format!("fetch body failed: {e}")))?;
        let html = String::from_utf8_lossy(&bytes);
        let text = html_to_text(&html);
        let mut out = text.trim().to_string();
        if out.chars().count() > MAX_FETCH_CHARS {
            out = out.chars().take(MAX_FETCH_CHARS).collect();
            out.push_str("\n…[truncated]");
        }
        if out.is_empty() {
            out = "(empty page)".into();
        }
        Ok(ToolResult {
            content: serde_json::Value::String(format!("{}\n\n---\n{}", params.url, out)),
            is_error: false,
            diff: None,
        })
    }
}

/// 轻量 HTML → 纯文本：去掉 script/style/head，标签转空格，合并空行。
fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut skip_depth = 0usize;
    let mut chars = html.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '<' {
            let tag = chars
                .by_ref()
                .take_while(|&c| c != '>')
                .collect::<String>();
            let lower = tag.to_ascii_lowercase();
            if lower.starts_with("script") || lower.starts_with("style") || lower.starts_with("head") {
                skip_depth += 1;
                continue;
            }
            if lower.starts_with("/script") || lower.starts_with("/style") || lower.starts_with("/head") {
                skip_depth = skip_depth.saturating_sub(1);
                continue;
            }
            if skip_depth == 0 {
                // 块级标签 → 换行
                if matches!(
                    lower.as_str(),
                    "p" | "div" | "br" | "li" | "h1" | "h2" | "h3" | "h4" | "pre"
                        | "tr" | "table" | "section" | "article" | "blockquote" | "hr"
                ) {
                    out.push('\n');
                } else {
                    out.push(' ');
                }
            }
            continue;
        }
        if skip_depth == 0 {
            out.push(c);
        }
    }
    out.lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}
