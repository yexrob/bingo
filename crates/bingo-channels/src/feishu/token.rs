//! The tenant access token over time (ADR-0016 §6).
//!
//! The endpoint caches for us — asking again with more than thirty minutes
//! left hands back the same token — so this cache exists for the QPS, not for
//! the freshness. It is single-flight all the same: eight calls that all find
//! the token stale should make one request, not eight.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::json;

use super::api::ApiError;

const PATH: &str = "/open-apis/auth/v3/tenant_access_token/internal";

/// Renew this long before the token expires. The issuer's own cache uses the
/// same window, so this asks for a new one exactly when it would mint one.
const MARGIN: Duration = Duration::from_secs(30 * 60);

#[derive(Clone, Debug)]
struct Cached {
    token: String,
    until: Instant,
}

pub struct Tokens {
    app_id: String,
    app_secret: String,
    cache: Mutex<Option<Cached>>,
    renewing: tokio::sync::Mutex<()>,
}

/// The app it signs for, never what it signs with.
impl std::fmt::Debug for Tokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tokens")
            .field("app_id", &self.app_id)
            .finish_non_exhaustive()
    }
}

impl Tokens {
    pub fn new(app_id: impl Into<String>, app_secret: impl Into<String>) -> Self {
        Self {
            app_id: app_id.into(),
            app_secret: app_secret.into(),
            cache: Mutex::new(None),
            renewing: tokio::sync::Mutex::new(()),
        }
    }

    pub fn app_id(&self) -> &str {
        &self.app_id
    }

    /// The bearer for the next request.
    pub async fn bearer(
        &self,
        http: &reqwest::Client,
        base: &str,
        now: Instant,
    ) -> Result<String, ApiError> {
        if let Some(fresh) = self.fresh(now) {
            return Ok(fresh);
        }
        let _renewing = self.renewing.lock().await;
        // Somebody else may have renewed it while this call waited.
        if let Some(fresh) = self.fresh(now) {
            return Ok(fresh);
        }
        let minted = self.mint(http, base).await?;
        *self.locked() = Some(minted.clone());
        Ok(minted.token)
    }

    fn fresh(&self, now: Instant) -> Option<String> {
        self.locked()
            .as_ref()
            .filter(|cached| cached.until > now)
            .map(|cached| cached.token.clone())
    }

    async fn mint(&self, http: &reqwest::Client, base: &str) -> Result<Cached, ApiError> {
        let body = json!({ "app_id": self.app_id, "app_secret": self.app_secret });
        let answer = super::api::request(http.post(format!("{base}{PATH}")).json(&body)).await?;
        let token = answer["tenant_access_token"]
            .as_str()
            .ok_or_else(|| ApiError::Transport("the token reply carries no token".into()))?;
        // Two hours is the ceiling; the reply's own number is the truth.
        let lifetime = Duration::from_secs(answer["expire"].as_u64().unwrap_or(7200));
        Ok(Cached {
            token: token.to_string(),
            until: Instant::now() + lifetime.saturating_sub(MARGIN),
        })
    }

    fn locked(&self) -> std::sync::MutexGuard<'_, Option<Cached>> {
        self.cache
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// An issuer that mints exactly once; a second request 404s, which is
    /// how a test proves that nothing asked twice.
    async fn issuer(expire: u64) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(PATH))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0,
                "msg": "ok",
                "tenant_access_token": "t-minted",
                "expire": expire,
            })))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        server
    }

    #[tokio::test]
    async fn a_fresh_token_is_used_without_asking_the_issuer_again() {
        let server = issuer(7200).await;
        let tokens = Tokens::new("cli_a", "secret");
        let http = reqwest::Client::new();
        let now = Instant::now();
        assert_eq!(
            tokens
                .bearer(&http, &server.uri(), now)
                .await
                .expect("a token"),
            "t-minted"
        );
        // The issuer mints once; a second request would 404 here.
        assert_eq!(
            tokens
                .bearer(&http, &server.uri(), now)
                .await
                .expect("a token"),
            "t-minted"
        );
    }

    #[tokio::test]
    async fn a_token_inside_the_renewal_margin_is_renewed() {
        let server = issuer(7200).await;
        let tokens = Tokens::new("cli_a", "secret");
        let http = reqwest::Client::new();
        tokens
            .bearer(&http, &server.uri(), Instant::now())
            .await
            .expect("a token");
        // Past the point where only the margin is left it is stale, so this
        // asks the issuer again — which has nothing left to give.
        let late = Instant::now() + Duration::from_secs(7200);
        assert!(tokens.bearer(&http, &server.uri(), late).await.is_err());
    }

    #[tokio::test]
    async fn eight_callers_at_once_make_one_request() {
        let server = issuer(7200).await;
        let tokens = std::sync::Arc::new(Tokens::new("cli_a", "secret"));
        let http = reqwest::Client::new();
        let now = Instant::now();
        let callers: Vec<_> = (0..8)
            .map(|_| {
                let (tokens, http, uri) = (tokens.clone(), http.clone(), server.uri());
                tokio::spawn(async move { tokens.bearer(&http, &uri, now).await })
            })
            .collect();
        for caller in callers {
            assert_eq!(
                caller.await.expect("a caller").expect("a token"),
                "t-minted"
            );
        }
    }

    #[test]
    fn the_debug_shape_never_carries_the_secret() {
        let shown = format!("{:?}", Tokens::new("cli_a", "the-secret"));
        assert!(shown.contains("cli_a"));
        assert!(!shown.contains("the-secret"), "{shown}");
    }
}
