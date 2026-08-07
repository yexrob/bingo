use std::time::Duration;

use futures_util::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};
use std::sync::Arc;
use thiserror::Error;

use crate::error::ErrorCode;

use super::sse::SseParser;
use super::types::{parse_sse_event, Request, StreamEvent, API_BASE, API_VERSION};
use super::types::{ContentBlock, DEFAULT_MAX_TOKENS, Role, SystemBlock};

pub const MAX_RETRIES: u32 = 5;

/// 请求整体超时（连接 + 首字节）：服务器无响应时结束等待而不是无限挂。
/// 用于 **agent 长回合**（流式）——不套用反馈层 10s/15s（回合中已有持续
/// 进度反馈），由本传输层超时 + 用户中断兜底（AC-53）。
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// 流式 body 空闲超时：连上之后服务端挂死（既不发事件也不断开）时，
/// headless 会永久阻塞——超过这个静默时长即判定断流。
const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// 短同步 **读** 操作反馈层超时（AC-12/14）：list_models / count_tokens 等。
/// 到点 drop future（底层 reqwest 连接随之取消）→ `TIMEOUT`，首要动作 = 重试。
const SHORT_READ_TIMEOUT: Duration = Duration::from_secs(10);

/// 短同步 **写** 操作反馈层超时（AC-13/14）：complete_text（非流式补全）等。
/// 到点 drop future → `TIMEOUT`。写路径 drop 对「服务端已应用写」是
/// best-effort，重试仍建议动作级幂等兜底（AC-15）。
const SHORT_WRITE_TIMEOUT: Duration = Duration::from_secs(15);

/// 400 上下文超限时重算输出预算的下限。
const FLOOR_OUTPUT_TOKENS: u32 = 3_000;

/// cfg(test) 测试钩子（#14 R3b 方案 A）：短同步操作反馈层超时的挂起注入，
/// 服务于 AC-12/13/14 到点行为 + AC-15 写幂等断言。默认 0 = 不挂起，
/// 无关测试不受影响；测试置 `set_hang` 后，三个短同步入口在 HTTP 发送前
/// 挂起对应时长，fake-timers（`start_paused`）下 advance 到超时点即触发
/// `timeout` → `TIMEOUT`。生产构建下 `maybe_hang` 直通，零开销。
#[cfg(test)]
pub(crate) mod test_hooks {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    /// 挂起时长 ms（0 = 直通）。
    static HANG_MS: AtomicU64 = AtomicU64::new(0);

    /// RAII guard：Drop 时清零挂起（panic 也不残留，防跨测试污染）。
    pub(crate) struct HangGuard;

    impl Drop for HangGuard {
        fn drop(&mut self) {
            HANG_MS.store(0, Ordering::Relaxed);
        }
    }

    /// 设置挂起并返回 guard。
    pub(crate) fn hang_guard(ms: u64) -> HangGuard {
        HANG_MS.store(ms, Ordering::Relaxed);
        HangGuard
    }

    pub(crate) fn hang() -> Duration {
        Duration::from_millis(HANG_MS.load(Ordering::Relaxed))
    }
}

/// 短同步入口的挂起包装：测试构建下先挂起（模拟慢网络/慢服务端），
/// 生产构建下直通。
#[cfg(test)]
async fn maybe_hang<F: std::future::Future>(inner: F) -> F::Output {
    tokio::time::sleep(test_hooks::hang()).await;
    inner.await
}

#[cfg(not(test))]
#[inline]
async fn maybe_hang<F: std::future::Future>(inner: F) -> F::Output {
    inner.await
}

#[derive(Debug, Error)]
pub enum ClientError {
    #[error("missing API key: set ANTHROPIC_API_KEY or DEEPSEEK_API_KEY")]
    MissingApiKey,
    #[error("invalid API key for HTTP header: {0}")]
    InvalidApiKey(String),
    #[error("API error: HTTP {status}: {body}")]
    Api { status: u16, body: String },
    #[error("API stream error: {0}")]
    Stream(String),
    #[error("transport error: {0}")]
    Transport(#[from] reqwest::Error),
    /// 服务器在 REQUEST_TIMEOUT 内无响应。
    #[error("request timed out after {REQUEST_TIMEOUT:?}")]
    Timeout,
}

impl ErrorCode for ClientError {
    /// 出口映射（见 `src/error.rs` 码表）：每个 variant 显式返回稳定码，
    /// match 穷尽无 `_` 臂——新增 variant 未处理即编译报错。
    fn error_code(&self) -> &'static str {
        match self {
            ClientError::MissingApiKey | ClientError::InvalidApiKey(_) => "AUTH_REQUIRED",
            ClientError::Api { status: 401, .. } => "AUTH_REQUIRED",
            ClientError::Api { status: 403, .. } => "PERMISSION_DENIED",
            ClientError::Api { status: 429, .. } => "RATE_LIMITED",
            // 其余非成功响应（4xx 非上述 / 5xx）：服务端交互异常，动作「稍后重试」。
            ClientError::Api { .. } => "SERVER_ERROR",
            ClientError::Stream(_) => "SERVER_ERROR",
            ClientError::Transport(_) => transport_offline_code(),
            ClientError::Timeout => "TIMEOUT",
        }
    }
}

/// Transport 臂映射锁定函数（`#[doc(hidden)]`，仅供防漂移单测断言）。
///
/// 为何需要：`reqwest::Error` 无公开构造（0.13.x 的 `new`/`builder` 等
/// 全为 `pub(crate)`），`ClientError::Transport` 变体无法在测试中运行时构造，
/// 防漂移单测不能直接枚举该变体——由此函数把「transport → OFFLINE」映射锁死。
#[doc(hidden)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn transport_offline_code() -> &'static str {
    "OFFLINE"
}

/// 当前生效的端点（/provider 切换时更新）。
#[derive(Debug, Clone)]
struct Endpoint {
    api_key: String,
    base_url: String,
    /// 该端点是否接受图片内容块（default 读顶层 sendImages，命名 provider 读 supportsImages）。
    supports_images: bool,
}

#[derive(Debug, Clone)]
pub struct Client {
    http: reqwest::Client,
    endpoint: Arc<std::sync::RwLock<Endpoint>>,
    /// 命名 provider 表（settings.providers；default 不在表内）。
    providers: std::collections::HashMap<String, Endpoint>,
}

impl Client {
    /// settings 优先，回落环境变量（ANTHROPIC_API_KEY/DEEPSEEK_API_KEY、
    /// ANTHROPIC_BASE_URL）。settings 与 env 都无 key 时报 MissingApiKey。
    pub fn from_settings(settings: &crate::settings::Settings) -> Result<Self, ClientError> {
        Self::from_settings_with(settings, |name| std::env::var(name))
    }

    /// from_settings 的可注入版（测试用假 env，避免改真实环境变量）。
    fn from_settings_with(
        settings: &crate::settings::Settings,
        env: impl Fn(&str) -> std::result::Result<String, std::env::VarError>,
    ) -> Result<Self, ClientError> {
        let api_key = settings
            .api_key
            .clone()
            .or_else(|| env("ANTHROPIC_API_KEY").ok())
            .or_else(|| env("DEEPSEEK_API_KEY").ok())
            .ok_or(ClientError::MissingApiKey)?;
        let base_url = settings.api_base_url.clone().unwrap_or_else(|| {
            env("ANTHROPIC_BASE_URL").unwrap_or_else(|_| API_BASE.to_string())
        });
        let mut providers = settings
            .providers
            .iter()
            .map(|(name, cfg)| {
                (
                    name.clone(),
                    Endpoint {
                        api_key: cfg.api_key.clone(),
                        base_url: cfg.api_base_url.clone(),
                        supports_images: cfg.supports_images.unwrap_or(false),
                    },
                )
            })
            .collect::<std::collections::HashMap<String, Endpoint>>();
        // default 端点也入 providers 表（key "default"）：set_provider /
        // with_provider("default") 走通（含「切回 default」），/model 二级
        // 对 default 拉列表用顶层端点、标签与内容一致（P0-C）。default 为
        // 保留名：顶层配置优先（后插入覆盖用户同名的 providers 定义）。
        let default_endpoint = Endpoint {
            api_key,
            base_url,
            supports_images: settings.send_images.unwrap_or(false),
        };
        providers.insert("default".to_string(), default_endpoint.clone());
        Ok(Self {
            http: reqwest::Client::new(),
            endpoint: Arc::new(std::sync::RwLock::new(default_endpoint)),
            providers,
        })
    }

    #[cfg(test)]
    pub fn new(api_key: String, base_url: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            endpoint: Arc::new(std::sync::RwLock::new(Endpoint {
                api_key,
                base_url,
                supports_images: false,
            })),
            providers: std::collections::HashMap::new(),
        }
    }

    /// 命名 provider 列表（不含 default；/provider 列出用，default 由
    /// 调用方显式补出——/model 菜单与 /provider 输出都以 "default" 打头）。
    pub fn provider_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .providers
            .keys()
            .filter(|n| n.as_str() != "default")
            .cloned()
            .collect();
        names.sort();
        names
    }

    /// 指定 provider 的端点（key/url；default = 顶层配置）。未知名字返回 None。
    pub fn provider_endpoint(&self, name: &str) -> Option<(String, String)> {
        self.providers.get(name).map(|e| (e.api_key.clone(), e.base_url.clone()))
    }

    /// 当前生效的 provider 端点（key/url 引用）。
    pub fn current_endpoint(&self) -> (String, String) {
        let e = self.endpoint.read().unwrap_or_else(|p| p.into_inner());
        (e.api_key.clone(), e.base_url.clone())
    }

    /// 当前端点是否接受图片内容块（`supportsImages`/`sendImages` 配置）。
    pub fn supports_images(&self) -> bool {
        self.endpoint.read().unwrap_or_else(|p| p.into_inner()).supports_images
    }

    /// 切换到命名 provider；未知名字报错（default = 顶层端点，永远可切回）。
    pub fn set_provider(&self, name: &str) -> Result<(), String> {
        let Some(endpoint) = self.providers.get(name).cloned() else {
            return Err(format!("未找到 provider \"{name}\"（/provider 查看列表）"));
        };
        *self.endpoint.write().unwrap_or_else(|p| p.into_inner()) = endpoint;
        Ok(())
    }

    /// 派生一个端点独立的 Client（子代理指定 provider 用）：新 Client
    /// 锁定该 provider 端点，providers 表共享（名字表一致）。不指定时
    /// 应直接 clone（共享端点，跟随父会话切换）。
    pub fn with_provider(&self, name: &str) -> Result<Client, String> {
        let endpoint = self
            .providers
            .get(name)
            .cloned()
            .ok_or_else(|| format!("未找到 provider \"{name}\"（/provider 查看列表）"))?;
        Ok(Client {
            http: self.http.clone(),
            endpoint: Arc::new(std::sync::RwLock::new(endpoint)),
            providers: self.providers.clone(),
        })
    }

    fn headers(&self) -> Result<HeaderMap, ClientError> {
        let endpoint = self.endpoint.read().unwrap_or_else(|p| p.into_inner());
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-api-key",
            HeaderValue::from_str(&endpoint.api_key)
                .map_err(|e| ClientError::InvalidApiKey(e.to_string()))?,
        );
        headers.insert(
            "anthropic-version",
            HeaderValue::from_static(API_VERSION),
        );
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        Ok(headers)
    }

    /// 发起流式请求，返回归一化事件流。
    pub async fn stream(
        &self,
        request: &Request,
    ) -> Result<impl futures_util::Stream<Item = Result<StreamEvent, ClientError>>, ClientError>
    {
        // 400 上下文超限重算需要修改 max_tokens → 克隆一份可变请求。
        let mut request = request.clone();
        let mut attempt = 0;
        let base_url = self.current_endpoint().1;
        loop {
            let builder = self
                .http
                .post(format!("{base_url}/v1/messages"))
                .headers(self.headers()?)
                .json(&request);
            match tokio::time::timeout(REQUEST_TIMEOUT, builder.send()).await {
                Ok(Ok(response)) if response.status().is_success() => {
                    return Ok(self.stream_body(response));
                }
                Ok(Ok(response)) if retryable(&response.status()) => {
                    let status = response.status();
                    let retry_after = response
                        .headers()
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.parse::<u64>().ok())
                        .map(Duration::from_secs);
                    let body = response.text().await.unwrap_or_default();
                    if attempt >= MAX_RETRIES {
                        return Err(ClientError::Api { status: status.as_u16(), body });
                    }
                    let delay = retry_after.unwrap_or_else(|| backoff(attempt));
                    tokio::time::sleep(delay).await;
                }
                Ok(Ok(response)) => {
                    let status = response.status();
                    let body = response.text().await.unwrap_or_default();
                    // 400 输出预算超限：按 "input length and max_tokens exceed context limit: A + B > C"
                    // 重算 max_tokens = max(3000, C − A − 1000) 重试一次。
                    if status.as_u16() == 400
                        && attempt == 0
                        && body.contains("exceed context limit")
                        && let Some((input, window)) = parse_context_limit(&body)
                        && let Some(recomputed) = window
                            .checked_sub(input)
                            .and_then(|rem| rem.checked_sub(1000))
                            .map(|rem| rem.max(FLOOR_OUTPUT_TOKENS as u64))
                            .map(|v| v.min(DEFAULT_MAX_TOKENS as u64))
                        && recomputed != request.max_tokens as u64
                    {
                        request.max_tokens = recomputed as u32;
                        attempt += 1;
                        continue;
                    }
                    return Err(ClientError::Api { status: status.as_u16(), body });
                }
                Ok(Err(_transport)) if attempt < MAX_RETRIES => {
                    tokio::time::sleep(backoff(attempt)).await;
                }
                Ok(Err(transport)) => return Err(ClientError::Transport(transport)),
                Err(_) => {
                    return Err(ClientError::Timeout);
                }
            }
            attempt += 1;
        }
    }

    /// 非流式补全：返回回复文本（compact 摘要、记忆提取用）。
    /// 与 stream 一致的退避重试：429/5xx 与瞬时 transport 错误不该直接
    /// 判定压缩失败（失败会累进熔断计数）。
    /// 短同步写操作：整个操作（含重试）套反馈层 15s（AC-13/14），
    /// 到点 drop future → `TIMEOUT`。
    pub async fn complete_text(
        &self,
        request: &Request,
    ) -> Result<String, ClientError> {
        tokio::time::timeout(SHORT_WRITE_TIMEOUT, maybe_hang(self.complete_text_inner(request)))
            .await
            .map_err(|_| ClientError::Timeout)?
    }

    /// complete_text 的内层实现（重试 + 响应解析）。由外层反馈层超时兜底，
    /// 单次网络发送不再单独套超时（外层 15s 是最强护栏）。
    async fn complete_text_inner(
        &self,
        request: &Request,
    ) -> Result<String, ClientError> {
        let mut request = request.clone();
        request.stream = false;
        let base_url = self.current_endpoint().1;
        let mut attempt = 0;
        let response = loop {
            let builder = self
                .http
                .post(format!("{base_url}/v1/messages"))
                .headers(self.headers()?)
                .json(&request);
            match builder.send().await {
                Ok(response) if response.status().is_success() => break response,
                Ok(response) if retryable(&response.status()) && attempt < MAX_RETRIES => {
                    let retry_after = response
                        .headers()
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.parse::<u64>().ok())
                        .map(Duration::from_secs);
                    tokio::time::sleep(retry_after.unwrap_or_else(|| backoff(attempt))).await;
                }
                Ok(response) => {
                    let status = response.status();
                    let body = response.text().await.unwrap_or_default();
                    return Err(ClientError::Api { status: status.as_u16(), body });
                }
                Err(_transport) if attempt < MAX_RETRIES => {
                    tokio::time::sleep(backoff(attempt)).await;
                }
                Err(transport) => return Err(ClientError::Transport(transport)),
            }
            attempt += 1;
        };
        let body: serde_json::Value = response.json().await?;
        let mut text = String::new();
        if let Some(blocks) = body.get("content").and_then(|c| c.as_array()) {
            for block in blocks {
                if let Some(t) = block
                    .get("text")
                    .and_then(|t| t.as_str())
                    .filter(|_| block.get("type").and_then(|t| t.as_str()) == Some("text"))
                {
                    text.push_str(t);
                }
            }
        }
        Ok(text)
    }

    /// 列出当前端点支持的模型（`GET {base}/v1/models`，Anthropic/DeepSeek 通用）：
    /// 返回 `data[].id`。`/model` 二级选择器异步拉取用。
    /// 短同步读操作：反馈层 10s（AC-12/14），到点 drop → `TIMEOUT`。
    pub async fn list_models(&self) -> Result<Vec<String>, ClientError> {
        let base_url = self.current_endpoint().1;
        let response = tokio::time::timeout(
            SHORT_READ_TIMEOUT,
            maybe_hang(
                self.http
                    .get(format!("{base_url}/v1/models"))
                    .headers(self.headers()?)
                    .send(),
            ),
        )
        .await
        .map_err(|_| ClientError::Timeout)??;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(ClientError::Api {
                status: status.as_u16(),
                body,
            });
        }
        let body: serde_json::Value = response.json().await?;
        let mut models: Vec<String> = body
            .get("data")
            .and_then(|d| d.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|m| m.get("id").and_then(|id| id.as_str()).map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        models.sort();
        Ok(models)
    }

    /// 输入 token 计数（D12：预算显示走官方 count_tokens API）。
    /// 短同步读操作：反馈层 10s（AC-12/14），到点 drop → `TIMEOUT`。
    pub async fn count_tokens(
        &self,
        model: &str,
        system: &[SystemBlock],
        messages: &[super::types::Message],
    ) -> Result<u64, ClientError> {
        let payload = serde_json::json!({
            "model": model,
            "system": system,
            "messages": messages,
        });
        let response = tokio::time::timeout(
            SHORT_READ_TIMEOUT,
            maybe_hang(
                self.http
                    .post(format!("{}/v1/messages/count_tokens", self.current_endpoint().1))
                    .headers(self.headers()?)
                    .json(&payload)
                    .send(),
            ),
        )
        .await
        .map_err(|_| ClientError::Timeout)??;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(ClientError::Api {
                status: status.as_u16(),
                body,
            });
        }
        let body: serde_json::Value = response.json().await?;
        Ok(body
            .get("input_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0))
    }

    fn stream_body(
        &self,
        response: reqwest::Response,
    ) -> impl futures_util::Stream<Item = Result<StreamEvent, ClientError>> {
        let mut parser = SseParser::new();
        let mut body = response.bytes_stream();
        async_stream::stream! {
            loop {
                let chunk = match next_with_idle(&mut body, STREAM_IDLE_TIMEOUT).await {
                    Ok(Some(Ok(chunk))) => chunk,
                    Ok(Some(Err(e))) => {
                        yield Err(ClientError::Transport(e));
                        return;
                    }
                    Ok(None) => break,
                    Err(e) => {
                        yield Err(e);
                        return;
                    }
                };
                let frames = match parser.feed(&chunk) {
                    Ok(frames) => frames,
                    Err(message) => {
                        yield Err(ClientError::Stream(message));
                        return;
                    }
                };
                for frame in frames {
                    match parse_sse_event(&frame.event, &frame.data) {
                        Ok(Some(event)) => yield Ok(event),
                        Ok(None) => {}
                        Err(message) => {
                            yield Err(ClientError::Stream(message));
                            return;
                        }
                    }
                }
            }
        }
    }
}

/// `stream.next()` 的空闲超时包装：idle 内一个事件都没有即判定断流，
/// 免得服务端挂死时 headless 永久阻塞。
async fn next_with_idle<S, T>(
    body: &mut S,
    idle: Duration,
) -> Result<Option<T>, ClientError>
where
    S: futures_util::Stream<Item = T> + Unpin,
{
    tokio::time::timeout(idle, body.next())
        .await
        .map_err(|_| ClientError::Stream(format!("no stream data for {idle:?}: server stalled")))
}

/// 指数退避 + jitter：500ms 起，cap 32s。
fn backoff(attempt: u32) -> Duration {
    let base_ms = (500u64 << attempt.min(6)).min(32_000);
    let jitter = rand_jitter(base_ms);
    Duration::from_millis(base_ms + jitter)
}

fn rand_jitter(scale: u64) -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    nanos % (scale / 2 + 1)
}

fn retryable(status: &reqwest::StatusCode) -> bool {
    status.is_server_error() || *status == reqwest::StatusCode::TOO_MANY_REQUESTS
}

/// 从 400 错误体解析 (input_tokens, context_window)：定位 "A + B > C"
/// 模式邻近的三个数字，而不是取全文倒数第三个（request-id 等会污染它）。
/// 单段解析失败（溢出/缺字段）只跳过该候选，不让整体变 None。
fn parse_context_limit(body: &str) -> Option<(u64, u64)> {
    // "input length and max_tokens exceed context limit: 12345 + 64000 > 200000"
    for (idx, _) in body.match_indices('>') {
        let Some(window) = leading_number(&body[idx + 1..]) else {
            continue;
        };
        let head = body[..idx].trim_end();
        let Some((_budget, digits)) = trailing_number(head) else {
            continue;
        };
        let Some(head) = head[..head.len() - digits].trim_end().strip_suffix('+') else {
            continue;
        };
        let Some((input, _)) = trailing_number(head.trim_end()) else {
            continue;
        };
        if input < window {
            return Some((input, window));
        }
    }
    None
}

/// 跳过前导空白后开头的整数。
fn leading_number(text: &str) -> Option<u64> {
    let text = text.trim_start();
    let digits = text.len() - text.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    text.get(..digits)?.parse().ok()
}

/// 结尾的整数及其字节长度。
fn trailing_number(text: &str) -> Option<(u64, usize)> {
    let digits = text.len() - text.trim_end_matches(|c: char| c.is_ascii_digit()).len();
    let value: u64 = text.get(text.len() - digits..)?.parse().ok()?;
    Some((value, digits))
}

/// 把一条完整 assistant 回复的流事件累积成回传消息。
#[derive(Debug, Default)]
pub struct AssistantAccumulator {
    pub content: Vec<ContentBlock>,
    pub stop_reason: Option<String>,
    in_flight: Option<InFlight>,
}

#[derive(Debug)]
enum InFlight {
    Text { text: String },
    Thinking { thinking: String, signature: String },
    ToolUse { id: String, name: String, input: String },
}

impl AssistantAccumulator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, event: &StreamEvent) -> Result<(), String> {
        match event {
            StreamEvent::TextStart { index } => {
                self.ensure_slot(*index)?;
                self.in_flight = Some(InFlight::Text { text: String::new() });
            }
            StreamEvent::ThinkingStart { index } => {
                self.ensure_slot(*index)?;
                self.in_flight = Some(InFlight::Thinking {
                    thinking: String::new(),
                    signature: String::new(),
                });
            }
            StreamEvent::ToolUseStart { index, id, name } => {
                self.ensure_slot(*index)?;
                self.in_flight = Some(InFlight::ToolUse {
                    id: id.clone(),
                    name: name.clone(),
                    input: String::new(),
                });
            }
            StreamEvent::TextDelta { index, text } => {
                self.push_delta(*index, |f| match f {
                    InFlight::Text { text: t } => {
                        t.push_str(text);
                        Ok(())
                    }
                    _ => Err("text delta for non-text block".into()),
                })?;
            }
            StreamEvent::ThinkingDelta { index, thinking } => {
                self.push_delta(*index, |f| match f {
                    InFlight::Thinking { thinking: t, .. } => {
                        t.push_str(thinking);
                        Ok(())
                    }
                    _ => Err("thinking delta for non-thinking block".into()),
                })?;
            }
            StreamEvent::SignatureDelta { index, signature } => {
                self.push_delta(*index, |f| match f {
                    InFlight::Thinking { signature: s, .. } => {
                        s.push_str(signature);
                        Ok(())
                    }
                    _ => Err("signature delta for non-thinking block".into()),
                })?;
            }
            StreamEvent::InputJsonDelta { index, partial_json } => {
                self.push_delta(*index, |f| match f {
                    InFlight::ToolUse { input, .. } => {
                        input.push_str(partial_json);
                        Ok(())
                    }
                    _ => Err("input_json delta for non-tool_use block".into()),
                })?;
            }
            StreamEvent::BlockStop { index } => {
                let f = self.in_flight.take().ok_or("block stop without start")?;
                let block = match f {
                    InFlight::Text { text } => ContentBlock::Text { text },
                    InFlight::Thinking { thinking, signature } => {
                        ContentBlock::Thinking { thinking, signature }
                    }
                    InFlight::ToolUse { id, name, input } => {
                        let input = serde_json::from_str(&input).unwrap_or(serde_json::Value::Null);
                        ContentBlock::ToolUse { id, name, input }
                    }
                };
                if self.content.len() == *index {
                    self.content.push(block);
                } else {
                    return Err(format!("block index gap at {index}"));
                }
            }
            StreamEvent::StopReason { stop_reason, .. } => {
                self.stop_reason = stop_reason.clone();
            }
            _ => {}
        }
        Ok(())
    }

    fn ensure_slot(&self, index: usize) -> Result<(), String> {
        if index != self.content.len() {
            return Err(format!("block start out of order: {index} != {}", self.content.len()));
        }
        Ok(())
    }

    fn push_delta(
        &mut self,
        _index: usize,
        f: impl FnOnce(&mut InFlight) -> Result<(), String>,
    ) -> Result<(), String> {
        let flight = self.in_flight.as_mut().ok_or("delta without block start")?;
        f(flight)
    }

    pub fn message(&self) -> super::types::Message {
        super::types::Message {
            role: Role::Assistant,
            content: self.content.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::types::parse_sse_event;
    use crate::api::types::StreamEvent;

    /// AC-12/13/14：短同步操作反馈层超时分档——读 10s / 写 15s 两档不混淆
    /// （读在 11s 前必报、写在 14s 前不报由常量值保证；list_models/count_tokens
    /// 用读档、complete_text 用写档见实现，stream 长回合不套反馈层见 AC-53）。
    #[test]
    fn feedback_timeout_tiers_are_read_10s_write_15s() {
        assert_eq!(SHORT_READ_TIMEOUT, Duration::from_secs(10));
        assert_eq!(SHORT_WRITE_TIMEOUT, Duration::from_secs(15));
        // 长回合传输层护栏保持 120s/60s（不套 10s/15s）。
        assert_eq!(REQUEST_TIMEOUT, Duration::from_secs(120));
        assert_eq!(STREAM_IDLE_TIMEOUT, Duration::from_secs(60));
    }

    /// AC-12 到点行为（方案 A + fake-timers）：短同步读操作 10s 到点落 `TIMEOUT`。
    #[tokio::test(start_paused = true)]
    async fn read_times_out_at_10s() {
        let _guard = test_hooks::hang_guard(60_000); // 挂起 60s，> 10s 读档
        let client = Client::new("k".into(), "https://example.com".into());
        let handle = tokio::spawn(async move { client.list_models().await });
        tokio::time::advance(Duration::from_secs(11)).await;
        let res = handle.await.unwrap();
        assert!(matches!(res, Err(ClientError::Timeout)), "读超时应落 TIMEOUT");
    }

    /// AC-13/14 到点行为 + 分档不混淆：写操作 14s 前必不报、15s 到点落 `TIMEOUT`。
    #[tokio::test(start_paused = true)]
    async fn write_times_out_at_15s_not_before_14s() {
        let _guard = test_hooks::hang_guard(60_000); // 挂起 60s，> 15s 写档
        let client = Client::new("k".into(), "https://example.com".into());
        let handle = tokio::spawn(async move {
            let req = crate::api::types::Request {
                model: "test".into(),
                max_tokens: 100,
                system: vec![],
                messages: vec![],
                tools: vec![],
                stream: false,
                thinking: None,
                output_config: None,
            };
            client.complete_text(&req).await
        });
        // AC-14：写在 14s 前不报（读档 10s 已过也不误伤写档）。
        tokio::time::advance(Duration::from_secs(14)).await;
        assert!(!handle.is_finished(), "写操作 14s 前不应超时");
        // 到 16s 触发写档。
        tokio::time::advance(Duration::from_secs(2)).await;
        let res = handle.await.unwrap();
        assert!(matches!(res, Err(ClientError::Timeout)), "写超时应落 TIMEOUT");
    }

    #[test]
    fn parses_context_limit_error() {
        let body = "400: input length and max_tokens exceed context limit: 12345 + 64000 > 200000";
        assert_eq!(parse_context_limit(body), Some((12345, 200000)));
    }

    #[test]
    fn from_settings_prefers_settings_over_env() {
        let settings = crate::settings::Settings {
            api_key: Some("sk-settings".into()),
            api_base_url: Some("https://settings.example".into()),
            ..Default::default()
        };
        let env = |name: &str| -> Result<String, std::env::VarError> {
            match name {
                "ANTHROPIC_API_KEY" => Ok("sk-env".into()),
                "ANTHROPIC_BASE_URL" => Ok("https://env.example".into()),
                _ => Err(std::env::VarError::NotPresent),
            }
        };
        let client = Client::from_settings_with(&settings, env).unwrap();
        assert_eq!(client.current_endpoint().0, "sk-settings");
        assert_eq!(client.current_endpoint().1, "https://settings.example");
    }

    #[test]
    fn from_settings_falls_back_to_env() {
        let settings = crate::settings::Settings::default();
        let env = |name: &str| -> Result<String, std::env::VarError> {
            match name {
                "DEEPSEEK_API_KEY" => Ok("sk-deepseek".into()),
                "ANTHROPIC_BASE_URL" => Ok("https://deepseek.example".into()),
                _ => Err(std::env::VarError::NotPresent),
            }
        };
        let client = Client::from_settings_with(&settings, env).unwrap();
        assert_eq!(client.current_endpoint().0, "sk-deepseek");
        assert_eq!(client.current_endpoint().1, "https://deepseek.example");
    }

    #[test]
    fn from_settings_missing_key_errors() {
        let settings = crate::settings::Settings::default();
        let env = |_name: &str| Err(std::env::VarError::NotPresent);
        assert!(matches!(
            Client::from_settings_with(&settings, env),
            Err(ClientError::MissingApiKey)
        ));
    }

    #[test]
    fn from_settings_defaults_base_url() {
        let settings = crate::settings::Settings {
            api_key: Some("sk".into()),
            ..Default::default()
        };
        let env = |_name: &str| Err(std::env::VarError::NotPresent);
        let client = Client::from_settings_with(&settings, env).unwrap();
        assert_eq!(client.current_endpoint().1, API_BASE);
    }

    #[test]
    fn provider_switch_changes_endpoint() {
        let mut settings = crate::settings::Settings {
            api_key: Some("sk-main".into()),
            ..Default::default()
        };
        settings.providers.insert(
            "deepseek".to_string(),
            crate::settings::ProviderConfig {
                api_key: "sk-ds".into(),
                api_base_url: "https://api.deepseek.com".into(),
                supports_images: None,
            },
        );
        settings.providers.insert(
            "local".to_string(),
            crate::settings::ProviderConfig {
                api_key: "sk-local".into(),
                api_base_url: "http://127.0.0.1:11434".into(),
                supports_images: None,
            },
        );
        let env = |_name: &str| Err(std::env::VarError::NotPresent);
        let client = Client::from_settings_with(&settings, env).unwrap();
        assert_eq!(client.current_endpoint().0, "sk-main");
        assert_eq!(client.provider_names(), vec!["deepseek", "local"]);

        client.set_provider("deepseek").unwrap();
        assert_eq!(client.current_endpoint().0, "sk-ds");
        assert_eq!(client.current_endpoint().1, "https://api.deepseek.com");

        // 切换后 headers 用新 key。
        let headers = client.headers().unwrap();
        assert_eq!(
            headers.get("x-api-key").unwrap().to_str().unwrap(),
            "sk-ds"
        );

        assert!(client.set_provider("nope").is_err(), "未知 provider 报错");
        // 未知 provider 不影响当前端点。
        assert_eq!(client.current_endpoint().0, "sk-ds");
    }

    /// P0-C：default 端点入 providers 表——provider_names 不含 default，
    /// set_provider/with_provider("default") 走通（切回顶层端点），
    /// provider_endpoint 可取 URL（/provider 列表展示用）。
    #[test]
    fn default_provider_is_switchable_and_listed_as_endpoint() {
        let mut settings = crate::settings::Settings {
            api_key: Some("sk-main".into()),
            api_base_url: Some("https://main.example".into()),
            ..Default::default()
        };
        settings.providers.insert(
            "deepseek".to_string(),
            crate::settings::ProviderConfig {
                api_key: "sk-ds".into(),
                api_base_url: "https://api.deepseek.com".into(),
                supports_images: None,
            },
        );
        let env = |_name: &str| Err(std::env::VarError::NotPresent);
        let client = Client::from_settings_with(&settings, env).unwrap();
        // default 不出现在命名列表（调用方显式补出）。
        assert_eq!(client.provider_names(), vec!["deepseek"]);
        assert_eq!(
            client.provider_endpoint("default"),
            Some(("sk-main".to_string(), "https://main.example".to_string()))
        );
        assert_eq!(client.provider_endpoint("deepseek").unwrap().1, "https://api.deepseek.com");
        assert_eq!(client.provider_endpoint("nope"), None);

        // 切到 deepseek 再切回 default：顶层端点恢复（含 supports_images）。
        client.set_provider("deepseek").unwrap();
        assert_eq!(client.current_endpoint().0, "sk-ds");
        client.set_provider("default").unwrap();
        assert_eq!(client.current_endpoint().0, "sk-main");
        assert_eq!(client.current_endpoint().1, "https://main.example");

        // with_provider("default") fork 出顶层端点（/model 二级对 default
        // 拉列表用，标签与内容一致）。
        let fork = client.with_provider("default").unwrap();
        assert_eq!(fork.current_endpoint().0, "sk-main");
    }

    /// supports_images：default 读顶层 sendImages；命名 provider 读各自
    /// supportsImages；切换端点时跟随。
    #[test]
    fn supports_images_follows_endpoint_switch() {
        let mut settings = crate::settings::Settings {
            api_key: Some("sk-main".into()),
            send_images: Some(true),
            ..Default::default()
        };
        settings.providers.insert(
            "vision".to_string(),
            crate::settings::ProviderConfig {
                api_key: "sk-v".into(),
                api_base_url: "https://vision.example".into(),
                supports_images: Some(true),
            },
        );
        settings.providers.insert(
            "text-only".to_string(),
            crate::settings::ProviderConfig {
                api_key: "sk-t".into(),
                api_base_url: "https://text.example".into(),
                supports_images: Some(false),
            },
        );
        let env = |_name: &str| Err(std::env::VarError::NotPresent);
        let client = Client::from_settings_with(&settings, env).unwrap();
        assert!(client.supports_images(), "default 读顶层 sendImages");

        client.set_provider("text-only").unwrap();
        assert!(!client.supports_images(), "显式 false 覆盖");
        client.set_provider("vision").unwrap();
        assert!(client.supports_images(), "supportsImages=true 生效");
    }

    #[test]
    fn rejects_malformed_context_limit() {
        assert_eq!(parse_context_limit("boom 42"), None);
        assert_eq!(parse_context_limit("400: overloaded"), None);
        // A >= C 不可能：保护性拒绝
        assert_eq!(parse_context_limit("900000 + 64000 > 200000"), None);
    }

    /// request-id 等无关数字与溢出数字都不得污染解析。
    #[test]
    fn context_limit_ignores_unrelated_numbers() {
        let body = concat!(
            r#"{"request_id":"req_0129384756","error":{"type":"invalid_request_error","#,
            r#""message":"input length and max_tokens exceed context limit: 150000 + 64000 > 200000"}}"#
        );
        assert_eq!(parse_context_limit(body), Some((150000, 200000)));

        // 溢出 u64 的无关数字只跳过该段，不让整体变 None。
        let overflowing = "trace 99999999999999999999999 \
             input length and max_tokens exceed context limit: 150000 + 64000 > 200000";
        assert_eq!(parse_context_limit(overflowing), Some((150000, 200000)));
    }

    /// 服务端连上后不再发事件：空闲超时判定断流，而不是永久阻塞。
    #[tokio::test]
    async fn idle_stream_times_out() {
        let mut stalled = futures_util::stream::pending::<u8>();
        let err = next_with_idle(&mut stalled, Duration::from_millis(50))
            .await
            .unwrap_err();
        assert!(matches!(&err, ClientError::Stream(m) if m.contains("server stalled")), "{err}");
    }

    #[tokio::test]
    async fn live_stream_passes_items_through() {
        let mut live = futures_util::stream::iter([1u8, 2]);
        let idle = Duration::from_secs(30);
        assert_eq!(next_with_idle(&mut live, idle).await.unwrap(), Some(1));
        assert_eq!(next_with_idle(&mut live, idle).await.unwrap(), Some(2));
        assert_eq!(
            next_with_idle(&mut live, idle).await.unwrap(),
            None,
            "流结束返回 None 而不是超时"
        );
    }

    fn ev(event: &str, data: &str) -> StreamEvent {
        parse_sse_event(event, data).unwrap().unwrap()
    }

    #[test]
    fn accumulates_text_and_thinking() {
        let mut acc = AssistantAccumulator::new();
        acc.push(&ev("content_block_start", r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}"#)).unwrap();
        acc.push(&ev("content_block_delta", r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"plan"}}"#)).unwrap();
        acc.push(&ev("content_block_delta", r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig123"}}"#)).unwrap();
        acc.push(&ev("content_block_stop", r#"{"type":"content_block_stop","index":0}"#)).unwrap();
        acc.push(&ev("content_block_start", r#"{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}"#)).unwrap();
        acc.push(&ev("content_block_delta", r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"hi"}}"#)).unwrap();
        acc.push(&ev("content_block_stop", r#"{"type":"content_block_stop","index":1}"#)).unwrap();
        assert_eq!(
            acc.content,
            vec![
                ContentBlock::Thinking { thinking: "plan".into(), signature: "sig123".into() },
                ContentBlock::Text { text: "hi".into() },
            ]
        );
    }

    #[test]
    fn accumulates_tool_use_input() {
        let mut acc = AssistantAccumulator::new();
        acc.push(&ev("content_block_start", r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"tu_9","name":"Bash","input":{}}}"#)).unwrap();
        acc.push(&ev("content_block_delta", r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"command\":"}}"#)).unwrap();
        acc.push(&ev("content_block_delta", r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"\"ls\"}"}}"#)).unwrap();
        acc.push(&ev("content_block_stop", r#"{"type":"content_block_stop","index":0}"#)).unwrap();
        assert_eq!(
            acc.content,
            vec![ContentBlock::ToolUse {
                id: "tu_9".into(),
                name: "Bash".into(),
                input: serde_json::json!({"command": "ls"}),
            }]
        );
    }

    #[test]
    fn tool_use_input_falls_back_to_null() {
        let mut acc = AssistantAccumulator::new();
        acc.push(&ev("content_block_start", r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"tu_1","name":"Bash","input":{}}}"#)).unwrap();
        acc.push(&ev("content_block_stop", r#"{"type":"content_block_stop","index":0}"#)).unwrap();
        assert!(matches!(&acc.content[0], ContentBlock::ToolUse { input, .. } if input.is_null()));
    }
}
