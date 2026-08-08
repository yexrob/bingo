//! OAuth for provider access (D33 §6): Codex/ChatGPT device flow + loopback
//! PKCE, lazy refresh with single-flight. Token persistence goes through the
//! `crate::auth::AuthStore` (auth.json, 0600, opencode-compatible shape).
//!
//! Endpoints verified against `openai/codex` source (`codex-rs/login/`) and
//! recorded in notes/research-oauth-cli.md.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use reqwest::header::CONTENT_TYPE;
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::auth::{AuthEntry, AuthStore};

/// Codex/ChatGPT OAuth client id (openai/codex, `codex-rs/login`).
pub const CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub const CODEX_ISSUER: &str = "https://auth.openai.com";

/// Eager-refresh lead time: a token is refreshed when it expires within this
/// window (opencode behavior), so requests rarely hit a 401.
const EAGER_REFRESH_LEAD: u64 = 300;

/// Device-flow total wait cap (codex: 15 minutes).
const DEVICE_MAX_WAIT: Duration = Duration::from_secs(15 * 60);

/// Loopback callback port (codex default; falls back on conflict).
const LOOPBACK_PORT: u16 = 1455;

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("oauth: HTTP {status}: {body}")]
    Http { status: u16, body: String },
    #[error("oauth transport: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("provider \"{0}\" 未登录：/provider login {0}")]
    NotLoggedIn(String),
    /// Refresh/revoke failed permanently (expired/reused/revoked token) —
    /// the stored auth is cleared and the user must log in again.
    #[error("登录已失效（{0}）：/provider login 重新登录")]
    RefreshPermanent(String),
    #[error("oauth: {0}")]
    Other(String),
    #[error("oauth timed out")]
    Timeout,
}

impl AuthError {
    fn http(status: u16, body: String) -> Self {
        // Permanent refresh failures get their own class (codex wording:
        // refresh_token_expired / _reused / _invalidated).
        if body.contains("refresh_token_expired")
            || body.contains("refresh_token_reused")
            || body.contains("refresh_token_invalidated")
        {
            AuthError::RefreshPermanent(body)
        } else {
            AuthError::Http { status, body }
        }
    }
}

impl From<crate::auth::AuthError> for AuthError {
    fn from(e: crate::auth::AuthError) -> Self {
        AuthError::Other(format!("auth store: {e}"))
    }
}

/// A logged-in token set (cached + persisted).
#[derive(Debug, Clone)]
pub struct TokenSet {
    pub access_token: String,
    pub refresh_token: String,
    pub id_token: Option<String>,
    /// Unix seconds at which the access token expires (None = unknown).
    pub expires_at: Option<u64>,
    /// Account/plan info (parsed from id_token claims where available).
    pub account_id: Option<String>,
}

impl TokenSet {
    fn from_tokens_json(json: &Value) -> TokenSet {
        let expires_in = json.get("expires_in").and_then(|v| v.as_u64());
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let id_token = json
            .get("id_token")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let access_token = json
            .get("access_token")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .into();
        TokenSet {
            access_token,
            refresh_token: json
                .get("refresh_token")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .into(),
            id_token,
            expires_at: expires_in.map(|e| now + e),
            // The token response may omit account_id — backfill from JWT
            // claims (opencode extractAccountId, D33 §6 / P2 codex variant).
            account_id: json
                .get("account_id")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .or_else(|| jwt_account_id(json.get("id_token").and_then(|v| v.as_str())))
                .or_else(|| jwt_account_id(json.get("access_token").and_then(|v| v.as_str()))),
        }
    }

    fn to_auth_entry(&self) -> AuthEntry {
        AuthEntry::Oauth {
            access: self.access_token.clone(),
            refresh: self.refresh_token.clone(),
            expires: self.expires_at.map(|e| e as i64).unwrap_or(0),
            account_id: self.account_id.clone(),
        }
    }

    fn from_auth_entry(entry: &AuthEntry) -> Option<TokenSet> {
        match entry {
            AuthEntry::Oauth {
                access,
                refresh,
                expires,
                account_id,
            } => Some(TokenSet {
                access_token: access.clone(),
                refresh_token: refresh.clone(),
                id_token: None,
                expires_at: (*expires > 0).then_some(*expires as u64),
                account_id: account_id.clone(),
            }),
            AuthEntry::Api { .. } => None,
        }
    }

    fn account_label(&self) -> Option<String> {
        self.account_id.clone()
    }

    /// Eager check: valid if it exists and expires beyond the lead window.
    fn is_fresh(&self) -> bool {
        match self.expires_at {
            Some(exp) => {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                exp.saturating_sub(now) > EAGER_REFRESH_LEAD
            }
            None => false, // no expiry info → refresh on first use
        }
    }
}

/// Per-provider OAuth flow endpoints (baked-in defaults for `codex`).
#[derive(Debug, Clone)]
pub struct OauthFlowConfig {
    pub client_id: String,
    pub issuer: String,
    /// Path (under issuer) of the device-code and poll endpoints.
    pub device_base: String,
    /// Path (under issuer) the user visits to enter the code.
    pub verify_path: String,
    pub token_path: String,
    pub revoke_path: String,
}

impl OauthFlowConfig {
    pub fn codex() -> Self {
        Self {
            client_id: CODEX_CLIENT_ID.into(),
            issuer: CODEX_ISSUER.into(),
            device_base: "/api/accounts/deviceauth".into(),
            verify_path: "/codex/device".into(),
            token_path: "/oauth/token".into(),
            revoke_path: "/oauth/revoke".into(),
        }
    }

    fn token_url(&self) -> String {
        format!("{}{}", self.issuer, self.token_path)
    }
}

/// One round of the codex device flow (verified endpoints):
/// 1. `POST {issuer}{device_base}/usercode {"client_id"}` → device_auth_id + user_code + interval
/// 2. the user enters the code at `{issuer}{verify_path}`
/// 3. poll `POST {issuer}{device_base}/token` (403/404 = pending) →
///    server-generated authorization_code + code_challenge + code_verifier
/// 4. exchange at the token endpoint (redirect_uri = `{issuer}/deviceauth/callback`)
pub struct DeviceFlow<'a> {
    http: &'a reqwest::Client,
    config: &'a OauthFlowConfig,
}

#[derive(Debug)]
pub struct DevicePrompt {
    pub verification_url: String,
    pub user_code: String,
}

impl<'a> DeviceFlow<'a> {
    pub fn new(http: &'a reqwest::Client, config: &'a OauthFlowConfig) -> Self {
        Self { http, config }
    }

    pub async fn start(&self) -> Result<(DevicePrompt, String, u64), AuthError> {
        let url = format!("{}{}/usercode", self.config.issuer, self.config.device_base);
        let body = serde_json::json!({ "client_id": self.config.client_id });
        let resp = self
            .http
            .post(url)
            .header(CONTENT_TYPE, "application/json")
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(AuthError::http(status.as_u16(), text));
        }
        let value: Value =
            serde_json::from_str(&text).map_err(|e| AuthError::Other(format!("usercode: {e}")))?;
        let device_auth_id = value
            .get("device_auth_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AuthError::Other("usercode missing device_auth_id".into()))?
            .to_string();
        let user_code = value
            .get("user_code")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AuthError::Other("usercode missing user_code".into()))?
            .to_string();
        let interval = value
            .get("interval")
            .and_then(|v| v.as_u64())
            .unwrap_or(5)
            .max(1);
        Ok((
            DevicePrompt {
                verification_url: format!("{}{}", self.config.issuer, self.config.verify_path),
                user_code,
            },
            device_auth_id,
            interval,
        ))
    }

    /// Poll until granted (or the 15-minute cap / denial).
    pub async fn poll(
        &self,
        device_auth_id: &str,
        user_code: &str,
        interval: u64,
    ) -> Result<TokenSet, AuthError> {
        let url = format!("{}{}/token", self.config.issuer, self.config.device_base);
        let deadline = SystemTime::now() + DEVICE_MAX_WAIT;
        loop {
            let body =
                serde_json::json!({ "device_auth_id": device_auth_id, "user_code": user_code });
            let resp = self
                .http
                .post(&url)
                .header(CONTENT_TYPE, "application/json")
                .json(&body)
                .send()
                .await?;
            let status = resp.status();
            if status.is_success() {
                let text = resp.text().await?;
                let value: Value = serde_json::from_str(&text)
                    .map_err(|e| AuthError::Other(format!("device token: {e}")))?;
                return self.exchange(&value).await;
            }
            if status == reqwest::StatusCode::FORBIDDEN || status == reqwest::StatusCode::NOT_FOUND
            {
                // Pending: user has not entered the code yet.
                if SystemTime::now() >= deadline {
                    return Err(AuthError::Timeout);
                }
                tokio::time::sleep(Duration::from_secs(interval)).await;
                continue;
            }
            let text = resp.text().await?;
            return Err(AuthError::http(status.as_u16(), text));
        }
    }

    /// Exchange the server-generated authorization code at the token
    /// endpoint (codex device flow shape).
    async fn exchange(&self, device_token: &Value) -> Result<TokenSet, AuthError> {
        let code = device_token
            .get("authorization_code")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AuthError::Other("device token missing authorization_code".into()))?;
        let code_verifier = device_token
            .get("code_verifier")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AuthError::Other("device token missing code_verifier".into()))?;
        let form = [
            ("grant_type", "authorization_code"),
            ("code", code),
            (
                "redirect_uri",
                &format!("{}/deviceauth/callback", self.config.issuer),
            ),
            ("client_id", &self.config.client_id),
            ("code_verifier", code_verifier),
        ];
        let resp = self
            .http
            .post(self.config.token_url())
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .form(&form)
            .send()
            .await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(AuthError::http(status.as_u16(), text));
        }
        let value: Value =
            serde_json::from_str(&text).map_err(|e| AuthError::Other(format!("token: {e}")))?;
        Ok(TokenSet::from_tokens_json(&value))
    }
}

/// PKCE helpers (S256; RFC 7636 verifier).
fn pkce_verifier() -> String {
    // 48 bytes → 64 URL-safe chars (within the 43..128 range). Seeded from
    // time + pid (no rand dep): adequate for a CLI one-shot verifier.
    random_urlsafe(48)
}

/// OAuth `state` nonce (CSRF, opencode codex.ts): random per authorize URL.
fn oauth_state() -> String {
    random_urlsafe(16)
}

/// N URL-safe random bytes. Zero-dep: each call mixes a fresh
/// `RandomState` hasher (per-call OS seed) with time + pid — adequate for
/// PKCE verifiers / OAuth state (local-attacker threat model, documented
/// trade-off; no crypto RNG dependency).
fn random_urlsafe(len: usize) -> String {
    use std::hash::{BuildHasher, Hasher};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut bytes = vec![0u8; len];
    for (i, b) in bytes.iter_mut().enumerate() {
        let seed = std::hash::RandomState::new().build_hasher().finish();
        let mix = seed.wrapping_mul(31).wrapping_add(i as u64 * 7)
            ^ (now as u64)
            ^ (std::process::id() as u64);
        *b = (mix >> (i % 64)) as u8 ^ (now >> (i % 8)) as u8;
    }
    URL_SAFE_NO_PAD.encode(&bytes)
}

fn pkce_challenge(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(digest)
}

/// Loopback PKCE flow (default for local terminals): local server on port
/// 1455 (fallback on conflict), browser opens the authorize URL, the
/// callback code is exchanged at the token endpoint.
pub struct LoopbackPkce<'a> {
    http: &'a reqwest::Client,
    config: &'a OauthFlowConfig,
}

impl<'a> LoopbackPkce<'a> {
    pub fn new(http: &'a reqwest::Client, config: &'a OauthFlowConfig) -> Self {
        Self { http, config }
    }

    /// Bind the callback server and build the authorize URL. Returns
    /// (url, redirect_uri, code_verifier, server_handle).
    pub async fn authorize_url(
        &self,
    ) -> Result<
        (
            String,
            String,
            String,
            tokio::task::JoinHandle<Result<TokenSet, AuthError>>,
        ),
        AuthError,
    > {
        let verifier = pkce_verifier();
        let challenge = pkce_challenge(&verifier);
        // Bind with port fallback.
        let mut port = LOOPBACK_PORT;
        let listener = loop {
            match tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
                Ok(l) => break l,
                Err(_) if port < LOOPBACK_PORT + 20 => {
                    port += 1;
                }
                Err(e) => return Err(AuthError::Other(format!("bind callback: {e}"))),
            }
        };
        let redirect_uri = format!("http://localhost:{port}/auth/callback");
        let state = oauth_state();
        // Codex/ChatGPT CLI login params (opencode codex.ts buildAuthorizeUrl):
        // codex_cli_simplified_flow is REQUIRED — its absence makes the
        // issuer route to the web flow → Authentication Error (main-reported).
        let url = format!(
            "{}/oauth/authorize?response_type=code&client_id={}&redirect_uri={}&scope=openid%20profile%20email%20offline_access&code_challenge={}&code_challenge_method=S256&codex_cli_simplified_flow=true&id_token_add_organizations=true&originator=bingo&state={}",
            self.config.issuer,
            self.config.client_id,
            urlencoding(&redirect_uri),
            challenge,
            state,
        );
        let http = self.http.clone();
        let config = self.config.clone();
        let redirect_uri_cb = redirect_uri.clone();
        let verifier_cb = verifier.clone();
        let expected_state = state.clone();
        let handle = tokio::spawn(async move {
            let (mut socket, _) = listener
                .accept()
                .await
                .map_err(|e| AuthError::Other(format!("accept callback: {e}")))?;
            let mut buf = vec![0u8; 4096];
            let n = tokio::io::AsyncReadExt::read(&mut socket, &mut buf)
                .await
                .map_err(|e| AuthError::Other(format!("read callback: {e}")))?;
            let head = String::from_utf8_lossy(&buf[..n]).to_string();
            // GET /auth/callback?code=...&state=... HTTP/1.1 — the state
            // nonce is validated (CSRF): a mismatch rejects the login.
            let (code, state) = head
                .split_whitespace()
                .nth(1)
                .and_then(|path| {
                    let (_, query) = path.split_once('?')?;
                    let mut code = None;
                    let mut state = None;
                    for kv in query.split('&') {
                        if let Some(v) = kv.strip_prefix("code=") {
                            code = Some(v);
                        } else if let Some(v) = kv.strip_prefix("state=") {
                            state = Some(v);
                        }
                    }
                    Some((code, state))
                })
                .ok_or_else(|| AuthError::Other("callback missing code".into()))?;
            let Some(code) = code else {
                return Err(AuthError::Other("callback missing code".into()));
            };
            if state != Some(expected_state.as_str()) {
                return Err(AuthError::Other("callback state mismatch".into()));
            }
            let code = urlencoding_decode(code);
            let ok = "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok";
            let _ = tokio::io::AsyncWriteExt::write_all(&mut socket, ok.as_bytes()).await;
            // Exchange.
            let form = [
                ("grant_type", "authorization_code"),
                ("code", &code),
                ("redirect_uri", &redirect_uri_cb),
                ("client_id", &config.client_id),
                ("code_verifier", &verifier_cb),
            ];
            let resp = http
                .post(config.token_url())
                .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
                .form(&form)
                .send()
                .await?;
            let status = resp.status();
            let text = resp.text().await?;
            if !status.is_success() {
                return Err(AuthError::http(status.as_u16(), text));
            }
            let value: Value =
                serde_json::from_str(&text).map_err(|e| AuthError::Other(format!("token: {e}")))?;
            Ok(TokenSet::from_tokens_json(&value))
        });
        Ok((url, redirect_uri, verifier, handle))
    }
}

fn urlencoding(s: &str) -> String {
    // Minimal percent-encoding for the redirect_uri in the authorize query.
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn urlencoding_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(hex) = hex
                && let Ok(v) = u8::from_str_radix(hex, 16)
            {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

/// Extract the ChatGPT account id from a JWT (opencode `extractAccountId`
/// semantics): `chatgpt_account_id` claim, or the same under
/// `https://api.openai.com/auth`, or the first organization id. Returns
/// None for non-JWTs / missing claims — the caller falls back to the
/// access token or omits the account header (codex semantics).
pub fn jwt_account_id(token: Option<&str>) -> Option<String> {
    let token = token?;
    let payload = token.split('.').nth(1)?;
    let decoded = URL_SAFE_NO_PAD.decode(payload).ok()?;
    let claims: Value = serde_json::from_slice(&decoded).ok()?;
    claims
        .get("chatgpt_account_id")
        .or_else(|| {
            claims
                .get("https://api.openai.com/auth")?
                .get("chatgpt_account_id")
        })
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .or_else(|| {
            claims
                .get("organizations")?
                .as_array()?
                .first()?
                .get("id")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
}

/// Minimal JWT for tests: base64url(header).base64url(payload).signature.
#[cfg(test)]
fn test_jwt(payload: &serde_json::Value) -> String {
    let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"none"}"#);
    let body = URL_SAFE_NO_PAD.encode(payload.to_string().as_bytes());
    format!("{header}.{body}.sig")
}

/// Refreshes access tokens and persists them via `AuthStore`: lazy + eager
/// (5 min lead), 401-triggered (`force_refresh`), single-flight so concurrent
/// streams do not stampede the token endpoint. Permanent failures clear the
/// stored auth and force re-login.
pub struct TokenProvider {
    pub provider_name: String,
    config: OauthFlowConfig,
    home: std::path::PathBuf,
    http: reqwest::Client,
    cache: Arc<tokio::sync::Mutex<Option<TokenSet>>>,
    refresh_lock: Arc<tokio::sync::Mutex<()>>,
    /// Sync account mirror for the trait's non-async `auth_status()`.
    account_mirror: Arc<std::sync::Mutex<Option<String>>>,
}

impl Clone for TokenProvider {
    fn clone(&self) -> Self {
        // Shared state by design: cache/lock/mirror are Arc'd; the store is
        // rebuilt from the same home (re-reads the same auth.json).
        Self {
            provider_name: self.provider_name.clone(),
            config: self.config.clone(),
            home: self.home.clone(),
            http: self.http.clone(),
            cache: self.cache.clone(),
            refresh_lock: self.refresh_lock.clone(),
            account_mirror: self.account_mirror.clone(),
        }
    }
}

impl std::fmt::Debug for TokenProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenProvider")
            .field("provider_name", &self.provider_name)
            .field("issuer", &self.config.issuer)
            .finish()
    }
}

impl TokenProvider {
    pub fn new(home: &std::path::Path, provider_name: &str, config: OauthFlowConfig) -> Self {
        let store = AuthStore::new(home);
        let cached = store
            .get(provider_name)
            .ok()
            .flatten()
            .and_then(|e| TokenSet::from_auth_entry(&e));
        let account = cached.as_ref().and_then(TokenSet::account_label);
        Self {
            provider_name: provider_name.to_string(),
            config,
            home: home.to_path_buf(),
            http: reqwest::Client::new(),
            cache: Arc::new(tokio::sync::Mutex::new(cached)),
            refresh_lock: Arc::new(tokio::sync::Mutex::new(())),
            account_mirror: Arc::new(std::sync::Mutex::new(account)),
        }
    }

    fn store(&self) -> AuthStore {
        AuthStore::new(&self.home)
    }

    /// Persist a freshly-obtained token set (from device flow / PKCE login).
    /// account_id is backfilled from JWT claims when the response omitted it
    /// (codex semantics — the ChatGPT-Account-Id header depends on it).
    pub async fn save(&self, tokens: &TokenSet) -> Result<(), AuthError> {
        let mut tokens = tokens.clone();
        if tokens.account_id.is_none() {
            tokens.account_id = jwt_account_id(tokens.id_token.as_deref())
                .or_else(|| jwt_account_id(Some(&tokens.access_token)));
        }
        self.store()
            .set(&self.provider_name, tokens.to_auth_entry())?;
        *self.cache.lock().await = Some(tokens.clone());
        *self
            .account_mirror
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = tokens.account_label();
        Ok(())
    }

    /// Current access token: cached fresh token, else refresh (single-flight).
    /// A missing cache re-reads the store first — `/provider login` writes
    /// auth.json while the session's provider instance is already built, so
    /// the next request picks the new credentials up without a reload.
    pub async fn access_token(&self) -> Result<String, AuthError> {
        {
            let cache = self.cache.lock().await;
            if let Some(t) = cache.as_ref()
                && t.is_fresh()
            {
                return Ok(t.access_token.clone());
            }
        }
        let tokens = self.refresh().await?;
        Ok(tokens.access_token)
    }

    /// Account label for the non-async `ProviderClient::auth_status`
    /// (None = not logged in).
    pub fn account_sync(&self) -> Option<String> {
        self.account_mirror
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// 401-triggered refresh: clear the cached token, refresh once.
    pub async fn force_refresh(&self) -> Result<TokenSet, AuthError> {
        *self.cache.lock().await = None;
        self.refresh().await
    }

    /// Single-flight refresh + persist.
    async fn refresh(&self) -> Result<TokenSet, AuthError> {
        let _guard = self.refresh_lock.lock().await;
        // Double-checked: another caller may have refreshed while we waited.
        {
            let cache = self.cache.lock().await;
            if let Some(t) = cache.as_ref()
                && t.is_fresh()
            {
                return Ok(t.clone());
            }
        }
        let tokens = {
            let mut cache = self.cache.lock().await;
            match cache.clone() {
                Some(t) => t,
                None => {
                    // Not cached: re-read the store (login may have just
                    // written it) before giving up.
                    let stored = self
                        .store()
                        .get(&self.provider_name)
                        .ok()
                        .flatten()
                        .and_then(|e| TokenSet::from_auth_entry(&e));
                    if let Some(t) = stored {
                        *cache = Some(t.clone());
                        t
                    } else {
                        return Err(AuthError::NotLoggedIn(self.provider_name.clone()));
                    }
                }
            }
        };
        let body = serde_json::json!({
            "client_id": self.config.client_id,
            "grant_type": "refresh_token",
            "refresh_token": tokens.refresh_token,
        });
        let resp = self
            .http
            .post(self.config.token_url())
            .header(CONTENT_TYPE, "application/json")
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            let err = AuthError::http(status.as_u16(), text);
            if matches!(err, AuthError::RefreshPermanent(_)) {
                // Stored refresh token is dead: clear auth, force re-login.
                let _ = self.store().remove(&self.provider_name);
                *self.cache.lock().await = None;
                *self
                    .account_mirror
                    .lock()
                    .unwrap_or_else(|p| p.into_inner()) = None;
            }
            return Err(err);
        }
        let value: Value =
            serde_json::from_str(&text).map_err(|e| AuthError::Other(format!("refresh: {e}")))?;
        let mut refreshed = TokenSet::from_tokens_json(&value);
        // The refresh token may rotate; keep the old one when absent.
        if refreshed.refresh_token.is_empty() {
            refreshed.refresh_token = tokens.refresh_token.clone();
        }
        if refreshed.id_token.is_none() {
            refreshed.id_token = tokens.id_token.clone();
        }
        if refreshed.account_id.is_none() {
            refreshed.account_id = tokens.account_id.clone();
        }
        self.save(&refreshed).await?;
        Ok(refreshed)
    }

    /// Log out: revoke the refresh token (best-effort) + clear storage.
    pub async fn logout(&self) -> Result<(), AuthError> {
        let tokens = self.cache.lock().await.clone();
        if let Some(t) = tokens {
            let url = format!("{}{}", self.config.issuer, self.config.revoke_path);
            let body =
                serde_json::json!({ "client_id": self.config.client_id, "token": t.refresh_token });
            let _ = self
                .http
                .post(url)
                .header(CONTENT_TYPE, "application/json")
                .json(&body)
                .send()
                .await;
        }
        let _ = self.store().remove(&self.provider_name);
        *self.cache.lock().await = None;
        *self
            .account_mirror
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = None;
        Ok(())
    }

    /// Whether the cache currently holds a token (tests + future UX).
    #[allow(dead_code)]
    pub fn is_logged_in(&self) -> bool {
        self.cache.try_lock().map(|c| c.is_some()).unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Minimal OAuth mock: usercode / device token poll / oauth token
    /// (authorization_code + refresh_token rotation) / revoke.
    struct MockOAuth {
        addr: String,
        poll_calls: Arc<AtomicUsize>,
        refresh_calls: Arc<AtomicUsize>,
    }

    async fn spawn_mock() -> MockOAuth {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let poll_calls = Arc::new(AtomicUsize::new(0));
        let refresh_calls = Arc::new(AtomicUsize::new(0));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let pc = poll_calls.clone();
        let rc = refresh_calls.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let mut buf = vec![0u8; 64 * 1024];
                let n = socket.read(&mut buf).await.unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]).to_string();
                let (status, body) = if head.contains("/deviceauth/usercode") {
                    (
                        "200 OK",
                        r#"{"device_auth_id":"da_1","user_code":"ABC-DEF","interval":1}"#,
                    )
                } else if head.contains("/deviceauth/token") {
                    let calls = pc.fetch_add(1, Ordering::SeqCst);
                    if calls == 0 {
                        ("403 Forbidden", r#"{"error":"pending"}"#)
                    } else {
                        (
                            "200 OK",
                            r#"{"authorization_code":"ac_1","code_challenge":"cc","code_verifier":"cv_1"}"#,
                        )
                    }
                } else if head.contains("/oauth/token") {
                    if head.contains("refresh_token") {
                        // refresh: first two calls succeed (rotation), the
                        // rest are permanent failures (revoked/expired).
                        let n = rc.fetch_add(1, Ordering::SeqCst);
                        if n >= 2 {
                            ("400 Bad Request", r#"{"error":"refresh_token_expired"}"#)
                        } else {
                            (
                                "200 OK",
                                r#"{"access_token":"at_2","refresh_token":"rt_2","expires_in":3600}"#,
                            )
                        }
                    } else if rc.fetch_add(1, Ordering::SeqCst) == 0 {
                        (
                            "200 OK",
                            r#"{"access_token":"at_1","refresh_token":"rt_1","expires_in":3600}"#,
                        )
                    } else {
                        ("400 Bad Request", r#"{"error":"refresh_token_expired"}"#)
                    }
                } else if head.contains("/oauth/revoke") {
                    ("200 OK", "{}")
                } else {
                    ("404 Not Found", "{}")
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
            }
        });
        MockOAuth {
            addr: format!("http://{addr}"),
            poll_calls,
            refresh_calls,
        }
    }

    fn codex_at(addr: &str) -> OauthFlowConfig {
        OauthFlowConfig {
            client_id: CODEX_CLIENT_ID.into(),
            issuer: addr.into(),
            device_base: "/api/accounts/deviceauth".into(),
            verify_path: "/codex/device".into(),
            token_path: "/oauth/token".into(),
            revoke_path: "/oauth/revoke".into(),
        }
    }

    fn home(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("bingo-api-auth-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("home")
    }

    fn tokens(access: &str, refresh: &str, expires_in: i64) -> TokenSet {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        TokenSet {
            access_token: access.into(),
            refresh_token: refresh.into(),
            id_token: None,
            expires_at: Some((now + expires_in) as u64),
            account_id: Some("acc_1".into()),
        }
    }

    /// Full device flow: usercode → poll (one pending, then granted) →
    /// exchange → TokenSet.
    #[tokio::test]
    async fn device_flow_full_lifecycle() {
        let mock = spawn_mock().await;
        let http = reqwest::Client::new();
        let config = codex_at(&mock.addr);
        let flow = DeviceFlow::new(&http, &config);
        let (prompt, device_auth_id, interval) = flow.start().await.unwrap();
        assert_eq!(prompt.user_code, "ABC-DEF");
        assert!(prompt.verification_url.ends_with("/codex/device"));
        let tokens = flow
            .poll(&device_auth_id, "ABC-DEF", interval)
            .await
            .unwrap();
        assert_eq!(tokens.access_token, "at_1");
        assert_eq!(tokens.refresh_token, "rt_1");
        assert!(tokens.expires_at.is_some());
        assert_eq!(
            mock.poll_calls.load(Ordering::SeqCst),
            2,
            "一次 pending 后成功"
        );
    }

    /// TokenProvider: fresh token served without refresh; expiry-driven
    /// refresh rotates + persists; force_refresh; permanent failure clears.
    #[tokio::test]
    async fn token_provider_refresh_lifecycle() {
        let mock = spawn_mock().await;
        let home = home("lifecycle");
        let config = codex_at(&mock.addr);

        // Fresh token: served from cache, no refresh.
        let provider = TokenProvider::new(&home, "codex", config.clone());
        provider
            .save(&tokens("at_fresh", "rt_1", 3600))
            .await
            .unwrap();
        assert_eq!(provider.access_token().await.unwrap(), "at_fresh");
        assert_eq!(
            mock.refresh_calls.load(Ordering::SeqCst),
            0,
            "新鲜 token 不刷新"
        );

        // Re-load with expired token → refresh (rotation) → persisted.
        let provider = TokenProvider::new(&home, "codex", config.clone());
        provider.save(&tokens("at_old", "rt_1", -10)).await.unwrap();
        let provider = TokenProvider::new(&home, "codex", config.clone());
        assert_eq!(provider.access_token().await.unwrap(), "at_2");
        assert_eq!(mock.refresh_calls.load(Ordering::SeqCst), 1);
        let stored = AuthStore::new(&home).get("codex").unwrap().unwrap();
        match stored {
            AuthEntry::Oauth {
                access, refresh, ..
            } => {
                assert_eq!(access, "at_2");
                assert_eq!(refresh, "rt_2", "refresh token 轮换已持久化");
            }
            _ => panic!("应为 oauth 条目"),
        }

        // 401 force_refresh: clears cache and refreshes once.
        assert_eq!(provider.force_refresh().await.unwrap().access_token, "at_2");
        assert_eq!(mock.refresh_calls.load(Ordering::SeqCst), 2);

        // Permanent failure: the stored auth is cleared (re-login required).
        assert!(matches!(
            provider.force_refresh().await,
            Err(AuthError::RefreshPermanent(_))
        ));
        assert!(
            AuthStore::new(&home).get("codex").unwrap().is_none(),
            "永久失败后清除存储"
        );
        assert!(!provider.is_logged_in());
        let _ = std::fs::remove_dir_all(home.parent().unwrap());
    }

    /// Single-flight: N concurrent access_token() calls on an expired cache
    /// trigger exactly one refresh.
    #[tokio::test]
    async fn token_provider_single_flight() {
        let mock = spawn_mock().await;
        let home = home("flight");
        let provider = Arc::new(TokenProvider::new(&home, "codex", codex_at(&mock.addr)));
        provider.save(&tokens("at_old", "rt_1", -10)).await.unwrap();
        let mut handles = Vec::new();
        for _ in 0..8 {
            let p = provider.clone();
            handles.push(tokio::spawn(async move { p.access_token().await.unwrap() }));
        }
        for h in handles {
            assert_eq!(h.await.unwrap(), "at_2");
        }
        assert_eq!(
            mock.refresh_calls.load(Ordering::SeqCst),
            1,
            "并发只刷新一次"
        );
        let _ = std::fs::remove_dir_all(home.parent().unwrap());
    }

    /// Not-logged-in: no stored token → access_token fails with NotLoggedIn.
    #[tokio::test]
    async fn token_provider_not_logged_in() {
        let home = home("nli");
        let provider = TokenProvider::new(&home, "codex", codex_at("http://x"));
        assert!(matches!(
            provider.access_token().await,
            Err(AuthError::NotLoggedIn(_))
        ));
        let _ = std::fs::remove_dir_all(home.parent().unwrap());
    }

    /// PKCE challenge is the base64url S256 digest of the verifier.
    #[test]
    fn pkce_challenge_is_s256_base64url() {
        let verifier = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcd";
        assert_eq!(
            pkce_challenge(verifier),
            URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
        );
        assert!(
            verifier.len() >= 43 && verifier.len() <= 128,
            "RFC 7636 长度"
        );
    }

    /// jwt_account_id: the three claim positions opencode checks, plus
    /// non-JWT input → None (the ChatGPT-Account-Id header is omitted).
    #[test]
    fn jwt_account_id_extracts_from_claims() {
        // ① top-level chatgpt_account_id.
        let t = test_jwt(&serde_json::json!({"chatgpt_account_id": "acc_1"}));
        assert_eq!(jwt_account_id(Some(&t)).as_deref(), Some("acc_1"));
        // ② nested https://api.openai.com/auth.
        let t = test_jwt(&serde_json::json!({
            "https://api.openai.com/auth": {"chatgpt_account_id": "acc_2"}
        }));
        assert_eq!(jwt_account_id(Some(&t)).as_deref(), Some("acc_2"));
        // ③ organizations[0].id.
        let t = test_jwt(&serde_json::json!({"organizations": [{"id": "org_3"}]}));
        assert_eq!(jwt_account_id(Some(&t)).as_deref(), Some("org_3"));
        // Precedence: top-level wins over the nested claim.
        let t = test_jwt(&serde_json::json!({
            "chatgpt_account_id": "acc_1",
            "https://api.openai.com/auth": {"chatgpt_account_id": "acc_2"}
        }));
        assert_eq!(jwt_account_id(Some(&t)).as_deref(), Some("acc_1"));
        // Non-JWT / missing claims → None.
        assert_eq!(jwt_account_id(Some("not-a-jwt")), None);
        assert_eq!(jwt_account_id(None), None);
        let t = test_jwt(&serde_json::json!({"sub": "u"}));
        assert_eq!(
            jwt_account_id(Some(&t)),
            None,
            "无 account claims 返回 None"
        );
    }

    /// 契约：loopback authorize URL 带 codex CLI 参数 + 随机 state（main 实测
    /// bug 回归——缺 codex_cli_simplified_flow → issuer 走 web 流 →
    /// Authentication Error）。
    #[tokio::test]
    async fn loopback_authorize_url_has_codex_params_and_random_state() {
        let http = reqwest::Client::new();
        let config = OauthFlowConfig::codex();
        let flow = LoopbackPkce::new(&http, &config);
        let (url, _redirect, _verifier, handle) = flow.authorize_url().await.unwrap();
        handle.abort();
        assert!(
            url.contains("codex_cli_simplified_flow=true"),
            "简化流参数: {url}"
        );
        assert!(
            url.contains("id_token_add_organizations=true"),
            "组织 claim 参数: {url}"
        );
        assert!(url.contains("originator=bingo"), "originator: {url}");
        let state = url.split("state=").nth(1).unwrap_or_default();
        assert!(
            !state.is_empty() && state != "bingo",
            "state 非固定: {state}"
        );
        // 两次调用 state 不同（CSRF 随机）。
        let (url2, _r2, _v2, handle2) = flow.authorize_url().await.unwrap();
        handle2.abort();
        let state2 = url2.split("state=").nth(1).unwrap_or_default();
        assert_ne!(state, state2, "state 应随机");
    }

    /// 契约：loopback 回调校验 state（CSRF）——错误 state 拒绝登录；
    /// 正确 state 通过校验进入 token 交换（本地无真实端点 → 网络错误，
    /// 而非 state 拒绝）。
    #[tokio::test]
    async fn loopback_callback_validates_state() {
        let http = reqwest::Client::new();
        let config = OauthFlowConfig::codex();
        let flow = LoopbackPkce::new(&http, &config);
        let (_url, redirect_uri, _v, handle) = flow.authorize_url().await.unwrap();
        // 错误 state → 拒绝。
        let _ = http
            .get(format!("{redirect_uri}?code=bad&state=WRONG"))
            .send()
            .await;
        let res = handle.await.unwrap();
        assert!(
            matches!(&res, Err(AuthError::Other(m)) if m.contains("state mismatch")),
            "错误 state 应拒绝: {res:?}"
        );

        // 正确 state → 通过校验（进入交换，真实端点不可达 → 网络错误）。
        let (url2, redirect_uri2, _v2, handle2) = flow.authorize_url().await.unwrap();
        let state2 = url2.split("state=").nth(1).unwrap();
        let _ = http
            .get(format!("{redirect_uri2}?code=good&state={state2}"))
            .send()
            .await;
        let res = handle2.await.unwrap();
        assert!(
            !matches!(&res, Err(AuthError::Other(m)) if m.contains("state mismatch")),
            "正确 state 不应被拒绝: {res:?}"
        );
        assert!(res.is_err(), "进入交换后本地端点不可达应报错: {res:?}");
    }

    /// Token responses without account_id are backfilled from JWT claims
    /// (opencode extractAccountId semantics) — the ChatGPT-Account-Id header
    /// and /provider account display depend on it.
    #[test]
    fn tokens_backfill_account_id_from_jwt() {
        let id_token = test_jwt(&serde_json::json!({
            "https://api.openai.com/auth": {"chatgpt_account_id": "acc_jwt"}
        }));
        let json = serde_json::json!({
            "access_token": "at",
            "refresh_token": "rt",
            "id_token": id_token,
            "expires_in": 3600,
        });
        let tokens = TokenSet::from_tokens_json(&json);
        assert_eq!(
            tokens.account_id.as_deref(),
            Some("acc_jwt"),
            "JWT 回填 account_id"
        );

        // Access-token fallback when id_token has no account claim.
        let access_token = test_jwt(&serde_json::json!({"chatgpt_account_id": "acc_at"}));
        let json = serde_json::json!({
            "access_token": access_token,
            "refresh_token": "rt",
            "expires_in": 3600,
        });
        let tokens = TokenSet::from_tokens_json(&json);
        assert_eq!(
            tokens.account_id.as_deref(),
            Some("acc_at"),
            "access token 回退"
        );

        // Explicit account_id wins over JWT claims.
        let json = serde_json::json!({
            "access_token": access_token,
            "refresh_token": "rt",
            "account_id": "acc_explicit",
            "expires_in": 3600,
        });
        let tokens = TokenSet::from_tokens_json(&json);
        assert_eq!(tokens.account_id.as_deref(), Some("acc_explicit"));
    }
}
