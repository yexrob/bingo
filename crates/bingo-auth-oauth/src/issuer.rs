//! Where one OAuth provider's endpoints live.
//!
//! Data, not constants: the library knows the shape of an issuer and nothing
//! about codex, so the provider crate that owns a variant owns its client id
//! too and a test can point the whole flow at a local mock. An issuer that
//! was *discovered* rather than written down (ADR-0050 §1) fills the same
//! shape: its endpoints are absolute, because RFC 8414 does not promise them
//! on the issuer's own host.

use crate::percent;

/// The code-on-another-screen flow, which is one issuer's own and not RFC
/// 8628: an issuer found over discovery has none.
#[derive(Clone, Debug)]
pub struct Device {
    /// Where a device code is minted, and where it is polled — the codex
    /// device flow uses two paths, not one.
    pub code_path: String,
    pub token_path: String,
    /// The page a person opens to enter the code.
    pub verify_path: String,
}

#[derive(Clone, Debug)]
pub struct Issuer {
    pub client_id: String,
    /// The issuer identifier. No trailing slash; a relative path below is
    /// joined to it verbatim.
    pub base: String,
    pub authorize_path: String,
    pub token_path: String,
    /// Absent when the issuer publishes no revocation endpoint: signing out
    /// then only forgets, which is what a person asked for either way.
    pub revoke_path: Option<String>,
    pub device: Option<Device>,
    /// Empty when nobody named one, and then no `scope` is sent at all.
    pub scope: String,
    /// RFC 8707: what the token is *for*, sent on authorize, token and
    /// refresh. An issuer that mints tokens for itself names no resource.
    pub resource: Option<String>,
    /// Whether the refresh and the revocation go as a form (RFC 6749 §6, RFC
    /// 7009), which is the standard and what a discovered issuer is sent.
    /// codex takes those two as JSON, and is the reason this is a field.
    pub form_encoded: bool,
    /// Authorize parameters this issuer needs beyond the standard set.
    pub authorize_extra: Vec<(String, String)>,
}

impl Issuer {
    /// A relative path hangs off the base; an absolute URL is already the
    /// endpoint and is used as it stands.
    pub fn url(&self, path: &str) -> String {
        if is_absolute(path) {
            return path.to_string();
        }
        format!("{}{path}", self.base.trim_end_matches('/'))
    }

    /// The URL the browser is sent to. `state` goes last so a person reading
    /// it in a terminal sees the parameters that mean something first.
    pub fn authorize_url(&self, redirect_uri: &str, challenge: &str, state: &str) -> String {
        let mut query = format!(
            "response_type=code&client_id={}&redirect_uri={}",
            percent::encode(&self.client_id),
            percent::encode(redirect_uri),
        );
        if !self.scope.is_empty() {
            query.push_str(&format!("&scope={}", percent::encode(&self.scope)));
        }
        query.push_str(&format!(
            "&code_challenge={}&code_challenge_method=S256",
            percent::encode(challenge)
        ));
        if let Some(resource) = &self.resource {
            query.push_str(&format!("&resource={}", percent::encode(resource)));
        }
        for (name, value) in &self.authorize_extra {
            query.push_str(&format!(
                "&{}={}",
                percent::encode(name),
                percent::encode(value)
            ));
        }
        query.push_str(&format!("&state={}", percent::encode(state)));
        format!("{}?{query}", self.url(&self.authorize_path))
    }

    /// The device flow's own redirect: the issuer generated the code itself,
    /// so the exchange still names a redirect it never called.
    pub fn device_redirect_uri(&self) -> String {
        self.url("/deviceauth/callback")
    }

    /// The page a person enters the code on, for an issuer that has one.
    pub fn verify_url(&self) -> Option<String> {
        self.device
            .as_ref()
            .map(|device| self.url(&device.verify_path))
    }
}

pub(crate) fn is_absolute(url: &str) -> bool {
    url.starts_with("https://") || url.starts_with("http://")
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// An issuer shaped like codex's: written down, with a device flow.
    pub(crate) fn issuer(base: &str) -> Issuer {
        Issuer {
            client_id: "app_TEST".into(),
            base: base.trim_end_matches('/').to_string(),
            authorize_path: "/oauth/authorize".into(),
            token_path: "/oauth/token".into(),
            revoke_path: Some("/oauth/revoke".into()),
            device: Some(Device {
                code_path: "/api/accounts/deviceauth/usercode".into(),
                token_path: "/api/accounts/deviceauth/token".into(),
                verify_path: "/codex/device".into(),
            }),
            scope: "openid profile email offline_access".into(),
            resource: None,
            form_encoded: false,
            authorize_extra: vec![
                ("codex_cli_simplified_flow".into(), "true".into()),
                ("originator".into(), "bingo".into()),
            ],
        }
    }

    #[test]
    fn the_authorize_url_is_the_one_the_issuer_expects() {
        assert_eq!(
            issuer("https://auth.example.com").authorize_url(
                "http://localhost:1455/auth/callback",
                "chal-1",
                "st-1"
            ),
            "https://auth.example.com/oauth/authorize\
             ?response_type=code\
             &client_id=app_TEST\
             &redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback\
             &scope=openid%20profile%20email%20offline_access\
             &code_challenge=chal-1\
             &code_challenge_method=S256\
             &codex_cli_simplified_flow=true\
             &originator=bingo\
             &state=st-1"
        );
    }

    /// The MCP shape (ADR-0050 §1): absolute endpoints, no extras, and the
    /// `resource` RFC 8707 asks for. Pinned as a literal so a parameter that
    /// stops being sent fails here rather than against a live server.
    #[test]
    fn a_discovered_issuer_names_its_resource_and_omits_a_scope_nobody_gave() {
        let discovered = Issuer {
            client_id: "cl_1".into(),
            base: "https://as.example.com".into(),
            authorize_path: "https://login.example.com/authorize".into(),
            token_path: "https://login.example.com/token".into(),
            revoke_path: None,
            device: None,
            scope: String::new(),
            resource: Some("https://mcp.example.com/api/mcp".into()),
            form_encoded: true,
            authorize_extra: Vec::new(),
        };
        assert_eq!(
            discovered.authorize_url("http://127.0.0.1:1455/auth/callback", "chal-1", "st-1"),
            "https://login.example.com/authorize\
             ?response_type=code\
             &client_id=cl_1\
             &redirect_uri=http%3A%2F%2F127.0.0.1%3A1455%2Fauth%2Fcallback\
             &code_challenge=chal-1\
             &code_challenge_method=S256\
             &resource=https%3A%2F%2Fmcp.example.com%2Fapi%2Fmcp\
             &state=st-1"
        );
        assert_eq!(discovered.verify_url(), None);
    }

    #[test]
    fn every_path_hangs_off_the_base_without_a_doubled_slash() {
        let issuer = Issuer {
            base: "https://auth.example.com/".into(),
            ..issuer("https://auth.example.com")
        };
        assert_eq!(
            issuer.url(&issuer.token_path),
            "https://auth.example.com/oauth/token"
        );
        assert_eq!(
            issuer.device_redirect_uri(),
            "https://auth.example.com/deviceauth/callback"
        );
        assert_eq!(
            issuer.verify_url().as_deref(),
            Some("https://auth.example.com/codex/device")
        );
    }

    #[test]
    fn an_absolute_endpoint_is_the_endpoint_and_the_base_is_not_prefixed() {
        let issuer = issuer("https://auth.example.com");
        assert_eq!(
            issuer.url("https://tokens.elsewhere.example/oauth2/v1/token"),
            "https://tokens.elsewhere.example/oauth2/v1/token"
        );
        assert_eq!(
            issuer.url("http://127.0.0.1:8080/token"),
            "http://127.0.0.1:8080/token"
        );
    }
}
