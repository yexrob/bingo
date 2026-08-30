//! Where one OAuth provider's endpoints live.
//!
//! Data, not constants: the library knows the shape of an issuer and nothing
//! about codex, so the provider crate that owns a variant owns its client id
//! too and a test can point the whole flow at a local mock.

use crate::percent;

#[derive(Clone, Debug)]
pub struct Issuer {
    pub client_id: String,
    /// No trailing slash; every path below is joined to it verbatim.
    pub base: String,
    pub authorize_path: String,
    pub token_path: String,
    pub revoke_path: String,
    /// Where a device code is minted, and where it is polled — the codex
    /// device flow is not RFC 8628 and uses two paths, not one.
    pub device_code_path: String,
    pub device_token_path: String,
    /// The page a person opens to enter the code.
    pub device_verify_path: String,
    pub scope: String,
    /// Authorize parameters this issuer needs beyond the standard set.
    pub authorize_extra: Vec<(String, String)>,
}

impl Issuer {
    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base.trim_end_matches('/'))
    }

    /// The URL the browser is sent to. `state` goes last so a person reading
    /// it in a terminal sees the parameters that mean something first.
    pub fn authorize_url(&self, redirect_uri: &str, challenge: &str, state: &str) -> String {
        let mut query = format!(
            "response_type=code&client_id={}&redirect_uri={}&scope={}&code_challenge={}&code_challenge_method=S256",
            percent::encode(&self.client_id),
            percent::encode(redirect_uri),
            percent::encode(&self.scope),
            percent::encode(challenge),
        );
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

    pub fn verify_url(&self) -> String {
        self.url(&self.device_verify_path)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// An issuer shaped like codex's, which is the only shape in the tree.
    pub(crate) fn issuer(base: &str) -> Issuer {
        Issuer {
            client_id: "app_TEST".into(),
            base: base.trim_end_matches('/').to_string(),
            authorize_path: "/oauth/authorize".into(),
            token_path: "/oauth/token".into(),
            revoke_path: "/oauth/revoke".into(),
            device_code_path: "/api/accounts/deviceauth/usercode".into(),
            device_token_path: "/api/accounts/deviceauth/token".into(),
            device_verify_path: "/codex/device".into(),
            scope: "openid profile email offline_access".into(),
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
        assert_eq!(issuer.verify_url(), "https://auth.example.com/codex/device");
    }
}
