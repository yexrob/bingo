use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde::Deserialize;

use super::{parse_input, Tool, ToolContext, ToolError, ToolResult};

/// 搜索请求超时。
const SEARCH_TIMEOUT: Duration = Duration::from_secs(20);
/// 单次搜索最多回填的结果数。
const MAX_RESULTS: usize = 8;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct WebSearchInput {
    #[schemars(description = "the search query")]
    pub query: String,
    #[serde(rename = "allowed_domains", default)]
    #[schemars(description = "only include results from these domains")]
    pub allowed_domains: Vec<String>,
    #[serde(rename = "blocked_domains", default)]
    #[schemars(description = "never include results from these domains")]
    pub blocked_domains: Vec<String>,
}

pub struct SearchHit {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

/// WebSearch：搜索网络（对标 Claude Code WebSearchTool）。
/// CC 依赖 Anthropic API 的服务器端 web_search 工具；bingo 自实现后端
/// （无 key 的 DuckDuckGo HTML 端点），回填格式对齐 CC：
/// "Web search results for query ... Links: [...] REMINDER: 必须附来源"。
pub struct WebSearchTool;

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> String {
        "WebSearch".into()
    }
    fn description(&self) -> String {
        let year = current_year();
        format!(
            "- Search the web and use the results to inform your response\n\
             - Provides up-to-date information beyond the knowledge cutoff\n\
             - Returns results with links as markdown hyperlinks\n\
             \n\
             CRITICAL: after answering, you MUST include a \"Sources:\" section at the end\n\
             listing all relevant URLs as [Title](URL) markdown hyperlinks.\n\
             \n\
             Use the current year ({year}) in search queries for recent information."
        )
    }
    fn input_schema(&self) -> serde_json::Value {
        super::schema_for::<WebSearchInput>()
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
        let params: WebSearchInput = parse_input(&input)?;
        if params.query.trim().len() < 2 {
            return Err(ToolError::failed("search query must be at least 2 characters"));
        }
        if !params.allowed_domains.is_empty() && !params.blocked_domains.is_empty() {
            return Err(ToolError::failed(
                "cannot specify both allowed_domains and blocked_domains",
            ));
        }
        let hits = search(&params).await?;
        let hits = filter_hits(hits, &params);

        let mut out = format!("Web search results for query: \"{}\"\n\n", params.query);
        if hits.is_empty() {
            out.push_str("No links found.\n\n");
        } else {
            let links: Vec<serde_json::Value> = hits
                .iter()
                .map(|h| {
                    serde_json::json!({
                        "title": h.title,
                        "url": h.url,
                        "snippet": h.snippet,
                    })
                })
                .collect();
            out.push_str(&format!(
                "Links: {}\n\n",
                serde_json::to_string(&links).unwrap_or_default()
            ));
        }
        out.push_str(
            "\nREMINDER: You MUST include the sources above in your response to the user \
             using markdown hyperlinks.",
        );
        Ok(ToolResult {
            content: serde_json::Value::String(out.trim().to_string()),
            is_error: false,
            diff: None,
        })
    }
}

fn current_year() -> u32 {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    (secs / 31_556_952 + 1970) as u32
}

/// DuckDuckGo HTML 端点搜索（无 key）。返回原始命中（未过滤）。
async fn search(params: &WebSearchInput) -> Result<Vec<SearchHit>, ToolError> {
    let client = reqwest::Client::new();
    let response = tokio::time::timeout(SEARCH_TIMEOUT, async {
        client
            .get("https://html.duckduckgo.com/html/")
            .query(&[("q", params.query.as_str())])
            .header(
                "User-Agent",
                "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0 Safari/537.36",
            )
            .header("Accept", "text/html,application/xhtml+xml")
            .send()
            .await
    })
    .await
    .map_err(|_| ToolError::failed("search timed out".to_string()))?
    .map_err(|e| ToolError::failed(format!("search failed: {e}")))?;
    if !response.status().is_success() {
        return Err(ToolError::failed(format!(
            "search failed: HTTP {}",
            response.status().as_u16()
        )));
    }
    let html = tokio::time::timeout(SEARCH_TIMEOUT, response.text())
        .await
        .map_err(|_| ToolError::failed("search body timed out".to_string()))?
        .map_err(|e| ToolError::failed(format!("search body failed: {e}")))?;
    Ok(parse_results(&html))
}

/// 解析 DDG HTML 结果块：`result__a`（标题/URL）与 `result__snippet`（摘要）。
fn parse_results(html: &str) -> Vec<SearchHit> {
    let mut hits = Vec::new();
    // 结果块以 <div class="result results_links..."> 或 result__body 分隔。
    // (?s)：a 标签与 snippet div 之间可能有换行。
    let re = regex::Regex::new(
        r#"(?s)class="result__a"[^>]*href="([^"]+)"[^>]*>(.*?)</a>.*?class="result__snippet"[^>]*>(.*?)</a>"#,
    )
    .unwrap();
    for cap in re.captures_iter(html) {
        let url = decode_entity(&cap[1]);
        let title = strip_tags(&decode_entity(&cap[2]));
        let snippet = strip_tags(&decode_entity(&cap[3]));
        hits.push(SearchHit { title, url, snippet });
    }
    hits
}

/// DDG 结果 URL 是重定向包装（//duckduckgo.com/l/?uddg=...）→ 解出真实 URL。
fn decode_entity(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find("&") {
        out.push_str(&rest[..start]);
        let entity = &rest[start..];
        let (repl, len) = if entity.starts_with("&#x") {
            entity
                .get(3..)
                .and_then(|hex| hex.split(';').next())
                .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                .and_then(char::from_u32)
                // parse 失败（不是合法的 &#x 实体）→ 只消费 "&" 本身。
                .map(|c| (c.to_string(), hex_len(entity)))
                .unwrap_or(("&".into(), 1))
        } else if entity.starts_with("&amp;") {
            ("&".into(), 5)
        } else if entity.starts_with("&quot;") {
            ("\"".into(), 6)
        } else if entity.starts_with("&#39;") {
            ("'".into(), 5)
        } else if entity.starts_with("&lt;") {
            ("<".into(), 4)
        } else if entity.starts_with("&gt;") {
            (">".into(), 4)
        } else if entity.starts_with("&nbsp;") {
            (" ".into(), 6)
        } else {
            ("&".into(), 1)
        };
        out.push_str(&repl);
        // entity 从 & 起，实体长度 len 直接按 entity 切（rest 可能更长）。
        rest = &entity[len..];
    }
    out.push_str(rest);
    out
}

fn hex_len(s: &str) -> usize {
    s.find(';').map(|i| i + 1).unwrap_or(s.len())
}

fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// allowed/blocked 域名过滤（对标 CC input schema 语义）。
fn filter_hits(hits: Vec<SearchHit>, params: &WebSearchInput) -> Vec<SearchHit> {
    hits.into_iter()
        .filter(|h| {
            let host = url::Url::parse(&h.url)
                .ok()
                .and_then(|u| u.host_str().map(|h| h.to_string()))
                .unwrap_or_default();
            if !params.allowed_domains.is_empty() {
                params
                    .allowed_domains
                    .iter()
                    .any(|d| host == *d || host.ends_with(&format!(".{d}")))
            } else {
                !params
                    .blocked_domains
                    .iter()
                    .any(|d| host == *d || host.ends_with(&format!(".{d}")))
            }
        })
        .take(MAX_RESULTS)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_duckduckgo_html() {
        let html = r#"
            <div class="result results_links">
              <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fpage&amp;rut=1">Example &amp; Page Title</a>
              <div class="result__snippet"><a class="result__snippet" href="...">First <b>snippet</b> text</a></div>
            </div>
            <div class="result">
              <a class="result__a" href="https://other.org/x">Other Site</a>
              <div class="result__snippet"><a class="result__snippet" href="...">second result</a></div>
            </div>
        "#;
        let hits = parse_results(html);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].title, "Example & Page Title");
        assert!(hits[0].url.contains("example.com"));
        assert_eq!(hits[0].snippet, "First snippet text");
        assert_eq!(hits[1].title, "Other Site");
    }

    #[test]
    fn filters_by_domain() {
        let hits = vec![
            SearchHit {
                title: "a".into(),
                url: "https://example.com/x".into(),
                snippet: String::new(),
            },
            SearchHit {
                title: "b".into(),
                url: "https://other.org/y".into(),
                snippet: String::new(),
            },
        ];
        let params = WebSearchInput {
            query: "q".into(),
            allowed_domains: vec!["example.com".into()],
            blocked_domains: Vec::new(),
        };
        let filtered = filter_hits(hits, &params);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].url, "https://example.com/x");
    }

    #[test]
    fn decodes_entities() {
        assert_eq!(decode_entity("a&amp;b"), "a&b");
        assert_eq!(decode_entity("&#x27;x&#x27;"), "'x'");
        assert_eq!(decode_entity("plain"), "plain");
    }
}
