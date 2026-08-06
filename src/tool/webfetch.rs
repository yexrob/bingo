use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde::Deserialize;

use super::{parse_input, Tool, ToolContext, ToolError, ToolResult};

/// 请求超时：60s。
const FETCH_TIMEOUT: Duration = Duration::from_secs(60);
/// 内容大小上限：10MB。
const MAX_CONTENT_BYTES: u64 = 10 * 1024 * 1024;
/// 抓取结果回填上限：100k 字符。
const MAX_FETCH_CHARS: usize = 100_000;
/// URL 长度上限：2000 字符。
const MAX_URL_LENGTH: usize = 2000;
/// 重定向上限：10 次。
const MAX_REDIRECTS: u32 = 10;

/// 缓存：URL 键、15 分钟 TTL、50MB 总量。
const CACHE_TTL: Duration = Duration::from_secs(15 * 60);
const CACHE_MAX_BYTES: u64 = 50 * 1024 * 1024;

struct CacheEntry {
    bytes: u64,
    content_type: String,
    content: String,
    fetched_at: u64,
}

struct FetchCache {
    entries: HashMap<String, CacheEntry>,
    total_bytes: u64,
}

static FETCH_CACHE: LazyLock<Mutex<FetchCache>> = LazyLock::new(|| {
    Mutex::new(FetchCache {
        entries: HashMap::new(),
        total_bytes: 0,
    })
});

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 缓存读写：先清过期条目，超总量时逐出最旧条目。
fn cache_get(url: &str) -> Option<CacheEntry> {
    let mut cache = FETCH_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let cutoff = now_secs().saturating_sub(CACHE_TTL.as_secs());
    cache.entries.retain(|_, e| e.fetched_at > cutoff);
    cache
        .entries
        .get(url)
        .map(|e| CacheEntry {
            bytes: e.bytes,
            content_type: e.content_type.clone(),
            content: e.content.clone(),
            fetched_at: e.fetched_at,
        })
}

fn cache_put(url: String, entry: CacheEntry) {
    let mut cache = FETCH_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(old) = cache.entries.remove(&url) {
        cache.total_bytes = cache.total_bytes.saturating_sub(old.bytes);
    }
    cache.entries.insert(url.clone(), entry);
    if cache.entries.len() > 1 {
        let cutoff = now_secs().saturating_sub(CACHE_TTL.as_secs());
        cache.entries.retain(|_, e| e.fetched_at > cutoff);
    }
    cache.total_bytes = cache
        .entries
        .values()
        .map(|e| e.bytes)
        .sum::<u64>();
    while cache.total_bytes > CACHE_MAX_BYTES && cache.entries.len() > 1 {
        let oldest = cache
            .entries
            .iter()
            .min_by_key(|(_, e)| e.fetched_at)
            .map(|(k, _)| k.clone());
        if let Some(k) = oldest {
            if let Some(evicted) = cache.entries.remove(&k) {
                cache.total_bytes = cache.total_bytes.saturating_sub(evicted.bytes);
            }
        } else {
            break;
        }
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct WebFetchInput {
    pub url: String,
    #[serde(default)]
    #[schemars(description = "what to extract from the page (accepted, content is returned verbatim)")]
    pub prompt: Option<String>,
}

/// WebFetch：抓取 URL → HTML 转 Markdown → 返回。
/// 简化：不引入次模型总结，返回原文；prompt 字段接受但忽略。二进制内容不落盘。
pub struct WebFetchTool;

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> String {
        "WebFetch".into()
    }
    fn description(&self) -> String {
        "Fetches content from a specified URL and returns it as Markdown.\n\
         - HTTP URLs are automatically upgraded to HTTPS\n\
         - Redirects to a different host are reported; refetch the new URL\n\
         - Results are truncated if very large\n\
         - Has a 15-minute cache for repeated fetches\n\
         - For GitHub URLs prefer the gh CLI via Bash instead\n\
         - If an MCP-provided fetch tool is available, prefer it"
            .into()
    }
    fn input_schema(&self) -> serde_json::Value {
        super::schema_for::<WebFetchInput>()
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
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let params: WebFetchInput = parse_input(&input)?;
        // prompt 语义是次模型总结；bingo 无次模型，接受但忽略。
        let _ = &params.prompt;
        validate_url(&params.url)?;
        let upgraded = upgrade_https(&params.url);

        if let Some(cached) = cache_get(&upgraded) {
            return Ok(ToolResult {
                content: serde_json::Value::String(format!(
                    "{}\n{} {}\n{}\n\n{}",
                    cached.content_type,
                    cached.bytes,
                    cached.content_type,
                    upgraded,
                    cached.content
                )),
                is_error: false,
                diff: None,
            });
        }

        let (content, bytes, code, code_text, content_type) =
            fetch(&ctx.http, &upgraded, 0).await?;

        let mut result = content;
        if result.chars().count() > MAX_FETCH_CHARS {
            result = result.chars().take(MAX_FETCH_CHARS).collect();
            result.push_str("\n…[content truncated]");
        }
        cache_put(
            upgraded.clone(),
            CacheEntry {
                bytes,
                content_type: content_type.clone(),
                content: result.clone(),
                fetched_at: now_secs(),
            },
        );
        let text = format!(
            "{upgraded}\nHTTP {code} {code_text} · {bytes} bytes · {content_type}\n\n{result}"
        );
        Ok(ToolResult {
            content: serde_json::Value::String(text),
            is_error: false,
            diff: None,
        })
    }
}

/// URL 校验：长度、无凭据、hostname 至少两段。
fn validate_url(url: &str) -> Result<(), ToolError> {
    if url.len() > MAX_URL_LENGTH {
        return Err(ToolError::failed(format!(
            "invalid URL: longer than {MAX_URL_LENGTH} characters"
        )));
    }
    let parsed = url::Url::parse(url)
        .map_err(|_| ToolError::failed(format!("invalid URL: {url}")))?;
    if !parsed.username().is_empty()
        || parsed.password().is_some_and(|s| !s.is_empty())
    {
        return Err(ToolError::failed("invalid URL: must not contain credentials"));
    }
    if parsed.host_str().is_some_and(|h| h.split('.').count() < 2) {
        return Err(ToolError::failed("invalid URL: hostname must have at least two parts"));
    }
    Ok(())
}

/// http → https 升级。
fn upgrade_https(url: &str) -> String {
    let mut parsed = match url.parse::<reqwest::Url>() {
        Ok(p) => p,
        Err(_) => return url.to_string(),
    };
    if parsed.scheme() == "http" {
        let _ = parsed.set_scheme("https");
    }
    parsed.to_string()
}

/// 流式读 body 并在读取过程中强制上限：无 Content-Length 的响应
/// 也不会先读满内存再检查（读超即断）。
async fn read_body_capped(response: reqwest::Response) -> Result<Vec<u8>, ToolError> {
    use futures_util::StreamExt;
    let read = async {
        let mut stream = response.bytes_stream();
        let mut buf: Vec<u8> = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| ToolError::failed(format!("fetch body failed: {e}")))?;
            if buf.len() as u64 + chunk.len() as u64 > MAX_CONTENT_BYTES {
                return Err(ToolError::failed(format!(
                    "content too large (exceeded limit {MAX_CONTENT_BYTES} bytes while streaming)"
                )));
            }
            buf.extend_from_slice(&chunk);
        }
        Ok(buf)
    };
    tokio::time::timeout(FETCH_TIMEOUT, read)
        .await
        .map_err(|_| ToolError::failed("fetch body timed out".to_string()))?
}

/// 重定向检查：协议/端口相同、无凭据、
/// hostname 去 www 后相同（路径可任意变化）。
fn is_permitted_redirect(original: &reqwest::Url, redirect: &reqwest::Url) -> bool {
    if redirect.scheme() != original.scheme() || redirect.port() != original.port() {
        return false;
    }
    if !redirect.username().is_empty()
        || redirect.password().is_some_and(|s| !s.is_empty())
    {
        return false;
    }
    original
        .host_str()
        .zip(redirect.host_str())
        .is_some_and(|(a, b)| strip_www(a) == strip_www(b))
}

fn strip_www(host: &str) -> &str {
    host.strip_prefix("www.").unwrap_or(host)
}

/// 跟随许可的重定向（同 host）；跨 host 返回提示文本让模型重新 fetch。
async fn fetch(
    client: &reqwest::Client,
    initial_url: &str,
    _depth: u32,
) -> Result<(String, u64, u16, String, String), ToolError> {
    let mut url = initial_url.to_string();
    let mut hops = 0u32;
    loop {
        if hops > MAX_REDIRECTS {
            return Err(ToolError::failed(format!(
                "too many redirects (exceeded {MAX_REDIRECTS})"
            )));
        }
        let response = tokio::time::timeout(FETCH_TIMEOUT, async {
            client
                .get(&url)
                .header(
                    "User-Agent",
                    "bingo-agent/0.1 (https://github.com/yexrob/bingo)",
                )
                .header("Accept", "text/markdown, text/html, */*")
                .send()
                .await
        })
        .await
        .map_err(|_| ToolError::failed(format!("fetch timed out: {url}")))?
        .map_err(|e| ToolError::failed(format!("fetch failed: {e}")))?;

        let status = response.status();
        if matches!(status.as_u16(), 301 | 302 | 307 | 308) {
            let location = response
                .headers()
                .get("location")
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| ToolError::failed("redirect missing Location header"))?
                .to_string();
            let redirect_url = url::Url::parse(&location)
                .or_else(|_| url::Url::parse(&url).and_then(|base| base.join(&location)));
            let redirect_url = redirect_url
                .map_err(|e| ToolError::failed(format!("bad redirect location: {e}")))?;
            let original = url::Url::parse(&url)
                .map_err(|e| ToolError::failed(format!("bad url: {e}")))?;
            if is_permitted_redirect(&original, &redirect_url) {
                url = redirect_url.to_string();
                hops += 1;
                continue;
            }
            let status_text = match status.as_u16() {
                301 => "Moved Permanently",
                308 => "Permanent Redirect",
                307 => "Temporary Redirect",
                _ => "Found",
            };
            return Ok((
                format!(
                    "REDIRECT DETECTED: the URL redirects to a different host.\n\
                     Original URL: {url}\n\
                     Redirect URL: {redirect_url}\n\
                     Status: {} {status_text}\n\n\
                     Fetch the redirected URL with WebFetch to continue.",
                    status.as_u16()
                ),
                0,
                status.as_u16(),
                status_text.to_string(),
                String::new(),
            ));
        }
        if !status.is_success() {
            return Err(ToolError::failed(format!(
                "fetch failed: HTTP {} for {url}",
                status.as_u16()
            )));
        }

        // 预检只在服务器给了 Content-Length 时有意义：
        // chunked 响应没有该头，曾被 unwrap_or(u64::MAX) 当成超大而 100% 失败。
        if let Some(len) = response.content_length()
            && len > MAX_CONTENT_BYTES
        {
            return Err(ToolError::failed(format!(
                "content too large ({len} bytes, limit {MAX_CONTENT_BYTES})"
            )));
        }
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let bytes = read_body_capped(response).await?;
        let html = String::from_utf8_lossy(&bytes).into_owned();
        let markdown = if content_type.contains("text/html") {
            html2md::parse_html(&html)
        } else {
            html
        };
        return Ok((
            markdown,
            bytes.len() as u64,
            status.as_u16(),
            status.canonical_reason().unwrap_or("").to_string(),
            content_type,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_bad_urls() {
        let long = format!("https://example.com/{}", "a".repeat(2100));
        assert!(validate_url(&long).is_err());
        assert!(validate_url("not a url").is_err());
        assert!(validate_url("https://user:pass@example.com/").is_err());
        assert!(validate_url("https://localhost/").is_err());
        assert!(validate_url("https://example.com/path").is_ok());
    }

    #[test]
    fn upgrades_http_to_https() {
        assert_eq!(
            upgrade_https("http://example.com/a"),
            "https://example.com/a"
        );
        assert_eq!(upgrade_https("https://example.com/a"), "https://example.com/a");
    }

    #[test]
    fn redirect_rules() {
        let a = reqwest::Url::parse("https://example.com/page").unwrap();
        let same = reqwest::Url::parse("https://example.com/other").unwrap();
        assert!(is_permitted_redirect(&a, &same));
        let www = reqwest::Url::parse("https://www.example.com/x").unwrap();
        assert!(is_permitted_redirect(&a, &www));
        let other = reqwest::Url::parse("https://evil.com/x").unwrap();
        assert!(!is_permitted_redirect(&a, &other));
        let http = reqwest::Url::parse("http://example.com/x").unwrap();
        assert!(!is_permitted_redirect(&a, &http));
    }

    /// 最小 HTTP 服务器：返回 chunked（无 Content-Length）响应。
    /// total_bytes 为要写出的正文总量。
    async fn chunked_server(total_bytes: usize) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap_or_else(|_| unreachable!());
        let addr = listener.local_addr().unwrap_or_else(|_| unreachable!());
        tokio::spawn(async move {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let mut discard = [0u8; 1024];
            let _ = socket.read(&mut discard).await;
            let header = "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\
                          Transfer-Encoding: chunked\r\n\r\n";
            if socket.write_all(header.as_bytes()).await.is_err() {
                return;
            }
            let chunk = vec![b'a'; 64 * 1024];
            let mut written = 0usize;
            while written < total_bytes {
                let size = chunk.len().min(total_bytes - written);
                if socket
                    .write_all(format!("{size:x}\r\n").as_bytes())
                    .await
                    .is_err()
                    || socket.write_all(&chunk[..size]).await.is_err()
                    || socket.write_all(b"\r\n").await.is_err()
                {
                    return;
                }
                written += size;
            }
            let _ = socket.write_all(b"0\r\n\r\n").await;
            let _ = socket.flush().await;
        });
        format!("http://{addr}/")
    }

    /// M2 回归：无 Content-Length 的 chunked 响应曾被当成超大而 100% 失败。
    #[tokio::test]
    async fn chunked_response_without_content_length_succeeds() {
        let url = chunked_server(4096).await;
        let (content, bytes, code, _, content_type) =
            fetch(&reqwest::Client::new(), &url, 0).await.unwrap();
        assert_eq!(code, 200);
        assert_eq!(bytes, 4096);
        assert_eq!(content.len(), 4096);
        assert!(content_type.contains("text/plain"));
    }

    /// M2 回归：上限在流式读取过程中强制，不是读完才检查。
    #[tokio::test]
    async fn oversized_streamed_body_is_rejected() {
        let url = chunked_server(MAX_CONTENT_BYTES as usize + 512 * 1024).await;
        let err = fetch(&reqwest::Client::new(), &url, 0)
            .await
            .expect_err("超限应报错");
        assert!(err.to_string().contains("too large"), "{err}");
    }

    #[test]
    fn cache_roundtrip_and_eviction() {
        let url = "https://cache-test.example.com/page";
        cache_put(
            url.to_string(),
            CacheEntry {
                bytes: 10,
                content_type: "text/html".into(),
                content: "hello".into(),
                fetched_at: now_secs(),
            },
        );
        let hit = cache_get(url).expect("cached entry should be found");
        assert_eq!(hit.content, "hello");
        // 过期条目被忽略
        cache_put(
            url.to_string(),
            CacheEntry {
                bytes: 10,
                content_type: "text/html".into(),
                content: "fresh".into(),
                fetched_at: now_secs().saturating_sub(20 * 60),
            },
        );
        assert!(cache_get(url).is_none());
    }
}
